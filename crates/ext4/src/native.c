#include <errno.h>
#include <fcntl.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include <ext2fs/ext2fs.h>

struct akimi_fs_info {
    uint32_t block_size;
    uint32_t first_inode;
    uint32_t inodes_per_group;
    uint32_t group_count;
    uint64_t inode_count;
    uint64_t allocated_inode_count;
    uint32_t feature_compat;
    uint32_t feature_incompat;
    uint32_t feature_ro_compat;
    uint8_t is_ext4;
};

struct akimi_inode {
    uint64_t inode;
    uint64_t logical_size;
    uint64_t allocated_size;
    uint32_t links;
    int64_t mtime;
    uint8_t kind;
};

typedef int (*akimi_inode_batch_callback)(const struct akimi_inode *,
                                          size_t, void *);
typedef int (*akimi_dir_batch_callback)(uint64_t, const uint64_t *,
                                        const uint32_t *, const uint16_t *,
                                        const uint8_t *, size_t, size_t,
                                        void *);

struct akimi_handle {
    ext2_filsys fs;
    struct akimi_inode *inode_batch;
    uint64_t *dir_children;
    uint32_t *dir_offsets;
    uint16_t *dir_lengths;
    uint8_t *dir_names;
    size_t dir_count;
    size_t dir_capacity;
    size_t dir_names_length;
    size_t dir_names_capacity;
};

enum {
    AKIMI_KIND_FILE = 0,
    AKIMI_KIND_DIRECTORY = 1,
    AKIMI_KIND_SYMLINK = 2,
    AKIMI_KIND_OTHER = 3,
};

enum {
    AKIMI_INODE_BATCH_SIZE = 1024,
    AKIMI_DIR_INITIAL_ENTRIES = 64,
    AKIMI_DIR_INITIAL_NAMES = 2048,
};

static uint8_t akimi_inode_kind(__u16 mode) {
    if (LINUX_S_ISREG(mode))
        return AKIMI_KIND_FILE;
    if (LINUX_S_ISDIR(mode))
        return AKIMI_KIND_DIRECTORY;
    if (LINUX_S_ISLNK(mode))
        return AKIMI_KIND_SYMLINK;
    return AKIMI_KIND_OTHER;
}

static int akimi_is_internal_inode(ext2_filsys fs, ext2_ino_t inode) {
    struct ext2_super_block *super = fs->super;
    if (inode != EXT2_ROOT_INO && inode < EXT2_FIRST_INODE(super))
        return 1;
    return (super->s_journal_inum != 0 && inode == super->s_journal_inum) ||
           (super->s_usr_quota_inum != 0 && inode == super->s_usr_quota_inum) ||
           (super->s_grp_quota_inum != 0 && inode == super->s_grp_quota_inum) ||
           (super->s_prj_quota_inum != 0 && inode == super->s_prj_quota_inum) ||
           (super->s_orphan_file_inum != 0 &&
            inode == super->s_orphan_file_inum);
}

/*
 * Returns 1 when no inode in [first, last] is allocated. This consults the
 * same inode bitmap that the per-inode filter below uses, so skipping such a
 * range cannot change scan results: every inode in it would fail the bitmap
 * test anyway. Only ENOENT (nothing set) means "clear"; any other outcome
 * falls back to scanning so unexpected errors never hide data.
 */
static int akimi_bitmap_range_clear(ext2_filsys fs, uint64_t first,
                                    uint64_t last) {
    ext2_ino_t found = 0;
    errcode_t error = ext2fs_find_first_set_inode_bitmap2(
        fs->inode_map, (ext2_ino_t)first, (ext2_ino_t)last, &found);
    if (error == 0)
        return 0;
    if (error == ENOENT)
        return 1;
    return 0;
}

static int64_t akimi_ext4_open_with_manager(const char *source,
                                            io_manager manager,
                                            struct akimi_fs_info *info,
                                            void **result) {
    ext2_filsys fs = NULL;
    errcode_t error = ext2fs_open(source,
                                  EXT2_FLAG_64BITS |
                                      EXT2_FLAG_SOFTSUPP_FEATURES,
                                  0, 0, manager, &fs);
    if (error)
        return error;

    struct akimi_handle *handle = calloc(1, sizeof(*handle));
    if (handle == NULL) {
        (void)ext2fs_close(fs);
        return ENOMEM;
    }
    handle->fs = fs;

    info->block_size = fs->blocksize;
    info->first_inode = EXT2_FIRST_INODE(fs->super);
    info->inodes_per_group = EXT2_INODES_PER_GROUP(fs->super);
    info->group_count = fs->group_desc_count;
    info->inode_count = fs->super->s_inodes_count;
    info->allocated_inode_count =
        fs->super->s_inodes_count - fs->super->s_free_inodes_count;
    info->feature_compat = fs->super->s_feature_compat;
    info->feature_incompat = fs->super->s_feature_incompat;
    info->feature_ro_compat = fs->super->s_feature_ro_compat;
    info->is_ext4 = ext2fs_has_feature_extents(fs->super) ? 1 : 0;
    *result = handle;
    return 0;
}

int64_t akimi_ext4_open(const char *path, struct akimi_fs_info *info,
                        void **result) {
    return akimi_ext4_open_with_manager(path, unix_io_manager, info, result);
}

int64_t akimi_ext4_open_fd(int fd, struct akimi_fs_info *info,
                           void **result) {
    int status = fcntl(fd, F_GETFL);
    if (status < 0)
        return errno;
    if ((status & O_ACCMODE) != O_RDONLY)
        return EACCES;

    char fd_name[32];
    int length = snprintf(fd_name, sizeof(fd_name), "%d", fd);
    if (length < 0 || (size_t)length >= sizeof(fd_name))
        return EINVAL;
    return akimi_ext4_open_with_manager(fd_name, unixfd_io_manager, info,
                                        result);
}

void akimi_ext4_close(void *handle) {
    if (handle == NULL)
        return;
    struct akimi_handle *akimi = handle;
    free(akimi->inode_batch);
    free(akimi->dir_children);
    free(akimi->dir_offsets);
    free(akimi->dir_lengths);
    free(akimi->dir_names);
    (void)ext2fs_close(akimi->fs);
    free(akimi);
}

int64_t akimi_ext4_load_inode_bitmap(void *handle) {
    struct akimi_handle *akimi = handle;
    return ext2fs_read_inode_bitmap(akimi->fs);
}

int64_t akimi_ext4_scan_inodes_batched(void *handle, uint64_t first_inode,
                                       uint64_t last_inode,
                                       akimi_inode_batch_callback callback,
                                       void *private_data) {
    struct akimi_handle *akimi = handle;
    ext2_filsys fs = akimi->fs;
    if (first_inode == 0 || first_inode > last_inode ||
        last_inode > fs->super->s_inodes_count)
        return EINVAL;
    if (fs->inode_map == NULL) {
        errcode_t load = ext2fs_read_inode_bitmap(fs);
        if (load)
            return load;
    }
    if (akimi->inode_batch == NULL) {
        akimi->inode_batch =
            malloc(AKIMI_INODE_BATCH_SIZE * sizeof(*akimi->inode_batch));
        if (akimi->inode_batch == NULL)
            return ENOMEM;
    }

    __u32 per_group = EXT2_INODES_PER_GROUP(fs->super);
    uint64_t first_group = (first_inode - 1) / per_group;
    uint64_t last_group = (last_inode - 1) / per_group;
    uint64_t group_total = fs->group_desc_count;

    int inode_size = EXT2_INODE_SIZE(fs->super);
    struct ext2_inode *inode = calloc(1, (size_t)inode_size);
    if (inode == NULL)
        return ENOMEM;

    size_t pending = 0;
    errcode_t error = 0;

    for (uint64_t group = first_group;
         group <= last_group && error == 0; group++) {
        if (group >= group_total)
            break;
        uint64_t group_first = group * per_group + 1;
        uint64_t group_last = (group + 1) * per_group;
        if (group_last > fs->super->s_inodes_count)
            group_last = fs->super->s_inodes_count;
        uint64_t range_start =
            group_first < first_inode ? first_inode : group_first;
        uint64_t range_end = group_last > last_inode ? last_inode : group_last;
        if (range_start > range_end)
            continue;
        /*
         * Skip the group's inode table blocks entirely when the bitmap
         * proves nothing is allocated. On filesystems with many free
         * inodes this avoids gigabytes of table reads.
         */
        if (akimi_bitmap_range_clear(fs, range_start, range_end))
            continue;

        /*
         * One fresh scan object per group with goto as the first
         * operation, mirroring the long-standing single-range pattern, so
         * positioning behavior is unchanged.
         */
        ext2_inode_scan scan = NULL;
        error = ext2fs_open_inode_scan(fs, 0, &scan);
        if (error)
            break;
        error = ext2fs_inode_scan_goto_blockgroup(scan, (dgrp_t)group);
        if (error) {
            ext2fs_close_inode_scan(scan);
            break;
        }

        for (;;) {
            ext2_ino_t inode_number = 0;
            errcode_t step = ext2fs_get_next_inode_full(scan, &inode_number,
                                                        inode, inode_size);
            if (step || inode_number == 0) {
                error = step;
                break;
            }
            if ((uint64_t)inode_number < range_start)
                continue;
            if ((uint64_t)inode_number > range_end)
                break;
            if (!ext2fs_test_inode_bitmap2(fs->inode_map, inode_number) ||
                inode->i_links_count == 0 ||
                akimi_is_internal_inode(fs, inode_number))
                continue;

            struct akimi_inode *slot = &akimi->inode_batch[pending++];
            slot->inode = inode_number;
            slot->logical_size = EXT2_I_SIZE(inode);
            slot->allocated_size = ext2fs_get_stat_i_blocks(fs, inode) * 512;
            slot->links = inode->i_links_count;
            slot->mtime = (int64_t)inode->i_mtime;
            slot->kind = akimi_inode_kind(inode->i_mode);
            if (pending == AKIMI_INODE_BATCH_SIZE) {
                if (callback(akimi->inode_batch, pending, private_data) !=
                    0) {
                    pending = 0;
                    error = ECANCELED;
                    break;
                }
                pending = 0;
            }
        }
        ext2fs_close_inode_scan(scan);
    }

    if (error == 0 && pending > 0) {
        if (callback(akimi->inode_batch, pending, private_data) != 0)
            error = ECANCELED;
    }

    free(inode);
    return error;
}

struct akimi_dir_state {
    struct akimi_handle *akimi;
    int aborted;
    int failed;
};

static int akimi_dir_buffer_reserve(struct akimi_dir_state *state,
                                    size_t extra_entries, size_t extra_names) {
    struct akimi_handle *akimi = state->akimi;
    if (akimi->dir_count + extra_entries > akimi->dir_capacity) {
        size_t capacity = akimi->dir_capacity == 0
                              ? AKIMI_DIR_INITIAL_ENTRIES
                              : akimi->dir_capacity;
        while (capacity < akimi->dir_count + extra_entries) {
            if (capacity > SIZE_MAX / (2 * sizeof(*akimi->dir_children)))
                return ENOMEM;
            capacity *= 2;
        }
        uint64_t *children =
            realloc(akimi->dir_children, capacity * sizeof(*children));
        uint32_t *offsets =
            realloc(akimi->dir_offsets, capacity * sizeof(*offsets));
        uint16_t *lengths =
            realloc(akimi->dir_lengths, capacity * sizeof(*lengths));
        if (children == NULL || offsets == NULL || lengths == NULL) {
            free(children);
            free(offsets);
            free(lengths);
            return ENOMEM;
        }
        akimi->dir_children = children;
        akimi->dir_offsets = offsets;
        akimi->dir_lengths = lengths;
        akimi->dir_capacity = capacity;
    }
    if (akimi->dir_names_length + extra_names > akimi->dir_names_capacity) {
        size_t capacity = akimi->dir_names_capacity == 0
                              ? AKIMI_DIR_INITIAL_NAMES
                              : akimi->dir_names_capacity;
        while (capacity < akimi->dir_names_length + extra_names) {
            if (capacity > SIZE_MAX / 2)
                return ENOMEM;
            capacity *= 2;
        }
        uint8_t *names = realloc(akimi->dir_names, capacity);
        if (names == NULL)
            return ENOMEM;
        akimi->dir_names = names;
        akimi->dir_names_capacity = capacity;
    }
    return 0;
}

static int akimi_directory_callback(ext2_ino_t directory, int entry,
                                    struct ext2_dir_entry *dirent,
                                    int offset, int blocksize, char *buffer,
                                    void *private_data) {
    (void)offset;
    (void)blocksize;
    (void)buffer;

    if (entry == DIRENT_DOT_FILE || entry == DIRENT_DOT_DOT_FILE ||
        dirent->inode == 0)
        return 0;

    struct akimi_dir_state *state = private_data;
    struct akimi_handle *akimi = state->akimi;
    int name_length = ext2fs_dirent_name_len(dirent);
    if (name_length < 0 || name_length > EXT2_NAME_LEN) {
        state->aborted = 1;
        return DIRENT_ABORT;
    }
    if (akimi->dir_names_length > UINT32_MAX) {
        state->aborted = 1;
        state->failed = 1;
        return DIRENT_ABORT;
    }
    if (akimi_dir_buffer_reserve(state, 1, (size_t)name_length) != 0) {
        state->aborted = 1;
        state->failed = 1;
        return DIRENT_ABORT;
    }
    size_t index = akimi->dir_count++;
    akimi->dir_children[index] = dirent->inode;
    akimi->dir_offsets[index] = (uint32_t)akimi->dir_names_length;
    akimi->dir_lengths[index] = (uint16_t)name_length;
    memcpy(akimi->dir_names + akimi->dir_names_length, dirent->name,
           (size_t)name_length);
    akimi->dir_names_length += (size_t)name_length;
    (void)directory;
    return 0;
}

int64_t akimi_ext4_scan_directory_batched(void *handle, uint64_t directory,
                                          akimi_dir_batch_callback callback,
                                          void *private_data) {
    struct akimi_handle *akimi = handle;
    struct akimi_dir_state state = {
        .akimi = akimi,
        .aborted = 0,
        .failed = 0,
    };
    akimi->dir_count = 0;
    akimi->dir_names_length = 0;
    errcode_t error = ext2fs_dir_iterate2(
        akimi->fs, (ext2_ino_t)directory, 0, NULL,
        akimi_directory_callback, &state);
    if (state.aborted)
        return state.failed ? ENOMEM : ECANCELED;
    if (error)
        return error;
    if (akimi->dir_count > 0) {
        if (callback(directory, akimi->dir_children, akimi->dir_offsets,
                     akimi->dir_lengths, akimi->dir_names,
                     akimi->dir_names_length, akimi->dir_count,
                     private_data) != 0)
            return ECANCELED;
    }
    return 0;
}

const char *akimi_ext4_error_message(int64_t error) {
    return error_message((errcode_t)error);
}
