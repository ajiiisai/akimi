#include <errno.h>
#include <linux/btrfs.h>
#include <sys/ioctl.h>

int akimi_btrfs_tree_search(int fd, void *args) {
    if (ioctl(fd, BTRFS_IOC_TREE_SEARCH_V2, args) == 0)
        return 0;
    return errno;
}
