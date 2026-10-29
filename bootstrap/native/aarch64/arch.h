#ifndef AXE_BOOTSTRAP_ARCH_H
#define AXE_BOOTSTRAP_ARCH_H

#define SYS_read 63
#define SYS_write 64
#define SYS_close 57
#define SYS_mmap 222
#define SYS_munmap 215
#define SYS_ppoll 73
#define SYS_socket 198
#define SYS_connect 203
#define SYS_sendto 206
#define SYS_setsockopt 208
#define SYS_getsockopt 209
#define SYS_fcntl 25
#define SYS_openat 56
#define SYS_unlinkat 35
#define SYS_fchmod 52
#define SYS_getpid 172
#define SYS_memfd_create 279
#define SYS_execve 221
#define SYS_execveat 281

static inline long axe_syscall6(long number, long arg1, long arg2, long arg3,
                                long arg4, long arg5, long arg6) {
    register long x8 __asm__("x8") = number;
    register long x0 __asm__("x0") = arg1;
    register long x1 __asm__("x1") = arg2;
    register long x2 __asm__("x2") = arg3;
    register long x3 __asm__("x3") = arg4;
    register long x4 __asm__("x4") = arg5;
    register long x5 __asm__("x5") = arg6;
    __asm__ volatile("svc 0"
                     : "+r"(x0)
                     : "r"(x8), "r"(x1), "r"(x2), "r"(x3), "r"(x4), "r"(x5)
                     : "memory", "cc");
    return x0;
}

#endif
