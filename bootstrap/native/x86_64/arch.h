#ifndef AXE_BOOTSTRAP_ARCH_H
#define AXE_BOOTSTRAP_ARCH_H

#define SYS_read 0
#define SYS_write 1
#define SYS_close 3
#define SYS_mmap 9
#define SYS_munmap 11
#define SYS_ppoll 271
#define SYS_socket 41
#define SYS_connect 42
#define SYS_sendto 44
#define SYS_setsockopt 54
#define SYS_getsockopt 55
#define SYS_fcntl 72
#define SYS_openat 257
#define SYS_unlinkat 263
#define SYS_fchmod 91
#define SYS_getpid 39
#define SYS_memfd_create 319
#define SYS_execve 59
#define SYS_execveat 322

static inline long axe_syscall6(long number, long arg1, long arg2, long arg3,
                                long arg4, long arg5, long arg6) {
    register long r10 __asm__("r10") = arg4;
    register long r8 __asm__("r8") = arg5;
    register long r9 __asm__("r9") = arg6;
    __asm__ volatile("syscall"
                     : "+a"(number)
                     : "D"(arg1), "S"(arg2), "d"(arg3), "r"(r10), "r"(r8),
                       "r"(r9)
                     : "rcx", "r11", "cc", "memory");
    return number;
}

#endif
