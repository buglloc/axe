#include "types.h"
#include "arch.h"

struct axe_address {
    axe_u8 family;
    axe_u8 bytes[16];
};

#include "constants.h"

_Static_assert(__BYTE_ORDER__ == __ORDER_LITTLE_ENDIAN__,
               "port byte swap assumes a little-endian target");
#define AXE_HTTP_PORT_NETWORK \
    ((axe_u16)(((AXE_HTTP_PORT & 0xff) << 8) | ((AXE_HTTP_PORT >> 8) & 0xff)))

#define AF_INET 2
#define AF_INET6 10
#define SOCK_STREAM 1
#define IPPROTO_TCP 6
#define SOL_SOCKET 1
#define SO_ERROR 4
#define SO_RCVTIMEO 20
#define SO_SNDTIMEO 21
#define F_SETFL 4
#define O_NONBLOCK 0x800
#define AT_FDCWD (-100)
#define O_WRONLY 1
#define O_CREAT 64
#define O_EXCL 128
#define O_CLOEXEC 0x80000
#define MFD_CLOEXEC 1
#define AT_EMPTY_PATH 0x1000
#define MSG_NOSIGNAL 0x4000
#define POLLOUT 0x4
#define POLLERR 0x8
#define POLLHUP 0x10
#define PROT_READ 1
#define PROT_WRITE 2
#define MAP_PRIVATE 2
#define MAP_ANONYMOUS 0x20

#define EINTR_CODE 4
#define EINPROGRESS_CODE 115
#define HTTP_HEADER_LIMIT 4096UL
#define CONNECT_TIMEOUT_MS 5000
#define SOCKET_TIMEOUT_SECONDS 5
#define MAX_SYSCALL_ERROR 4095L

struct axe_timeval {
    axe_s64 tv_sec;
    axe_s64 tv_usec;
};

struct axe_timespec {
    axe_s64 tv_sec;
    axe_s64 tv_nsec;
};

struct axe_pollfd {
    int fd;
    short events;
    short revents;
};

struct axe_sockaddr_in {
    axe_u16 family;
    axe_u16 port;
    axe_u32 address;
    axe_u8 zero[8];
};

struct axe_sockaddr_in6 {
    axe_u16 family;
    axe_u16 port;
    axe_u32 flowinfo;
    axe_u8 address[16];
    axe_u32 scope_id;
};

union axe_sockaddr_buffer {
    struct axe_sockaddr_in ipv4;
    struct axe_sockaddr_in6 ipv6;
};

static int axe_syscall_failed(long result) {
    return result < 0 && result >= -MAX_SYSCALL_ERROR;
}

static void axe_zero(axe_u8 *buffer, axe_usize length) {
    while (length != 0) {
        *buffer++ = 0;
        --length;
    }
}

static void axe_copy(axe_u8 *destination, const axe_u8 *source, axe_usize length) {
    while (length != 0) {
        *destination++ = *source++;
        --length;
    }
}

static int axe_equal(const axe_u8 *left, const axe_u8 *right, axe_usize length) {
    axe_u8 difference = 0;
    while (length != 0) {
        difference |= *left++ ^ *right++;
        --length;
    }
    return difference == 0;
}

static int axe_write_all(long fd, const axe_u8 *buffer, axe_usize length) {
    while (length != 0) {
        long written = axe_syscall6(SYS_write, fd, (long)buffer, (long)length, 0,
                                    0, 0);
        if (written < 0) {
            if (written == -EINTR_CODE) {
                continue;
            }
            return -1;
        }
        if (written == 0 || (axe_usize)written > length) {
            return -1;
        }
        buffer += (axe_usize)written;
        length -= (axe_usize)written;
    }
    return 0;
}

static int axe_send_all(long fd, const axe_u8 *buffer, axe_usize length) {
    while (length != 0) {
        long sent = axe_syscall6(SYS_sendto, fd, (long)buffer, (long)length,
                                 MSG_NOSIGNAL, 0, 0);
        if (sent < 0) {
            if (sent == -EINTR_CODE) {
                continue;
            }
            return -1;
        }
        if (sent == 0 || (axe_usize)sent > length) {
            return -1;
        }
        buffer += (axe_usize)sent;
        length -= (axe_usize)sent;
    }
    return 0;
}

static int axe_set_socket_timeouts(long fd) {
    struct axe_timeval timeout;
    timeout.tv_sec = SOCKET_TIMEOUT_SECONDS;
    timeout.tv_usec = 0;
    if (axe_syscall6(SYS_setsockopt, fd, SOL_SOCKET, SO_RCVTIMEO,
                     (long)&timeout, (long)sizeof(timeout), 0) < 0) {
        return -1;
    }
    if (axe_syscall6(SYS_setsockopt, fd, SOL_SOCKET, SO_SNDTIMEO,
                     (long)&timeout, (long)sizeof(timeout), 0) < 0) {
        return -1;
    }
    return 0;
}

static int axe_connect(const struct axe_address *address) {
    long fd = axe_syscall6(SYS_socket, address->family, SOCK_STREAM, IPPROTO_TCP,
                           0, 0, 0);
    if (axe_syscall_failed(fd)) {
        return -1;
    }
    if (axe_syscall6(SYS_fcntl, fd, F_SETFL, O_NONBLOCK, 0, 0, 0) < 0) {
        axe_syscall6(SYS_close, fd, 0, 0, 0, 0, 0);
        return -1;
    }

    union axe_sockaddr_buffer endpoint;
    axe_zero((axe_u8 *)&endpoint, sizeof(endpoint));
    axe_usize address_length;
    if (address->family == AF_INET6) {
        endpoint.ipv6.family = AF_INET6;
        endpoint.ipv6.port = AXE_HTTP_PORT_NETWORK;
        axe_copy(endpoint.ipv6.address, address->bytes, 16);
        address_length = sizeof(endpoint.ipv6);
    } else {
        endpoint.ipv4.family = AF_INET;
        endpoint.ipv4.port = AXE_HTTP_PORT_NETWORK;
        axe_copy((axe_u8 *)&endpoint.ipv4.address, address->bytes, 4);
        address_length = sizeof(endpoint.ipv4);
    }

    long connected = axe_syscall6(SYS_connect, fd, (long)&endpoint,
                                  (long)address_length, 0, 0, 0);
    if (connected < 0 && connected != -EINPROGRESS_CODE) {
        axe_syscall6(SYS_close, fd, 0, 0, 0, 0, 0);
        return -1;
    }
    if (connected == -EINPROGRESS_CODE) {
        struct axe_pollfd pollfd;
        struct axe_timespec timeout;
        pollfd.fd = (int)fd;
        pollfd.events = POLLOUT;
        pollfd.revents = 0;
        timeout.tv_sec = CONNECT_TIMEOUT_MS / 1000;
        timeout.tv_nsec = (CONNECT_TIMEOUT_MS % 1000) * 1000000;
        long polled = axe_syscall6(SYS_ppoll, (long)&pollfd, 1,
                                   (long)&timeout, 0, 0, 0);
        if (polled <= 0 ||
            (pollfd.revents & (POLLOUT | POLLERR | POLLHUP)) == 0) {
            axe_syscall6(SYS_close, fd, 0, 0, 0, 0, 0);
            return -1;
        }
        int socket_error = 0;
        axe_u32 error_length = sizeof(socket_error);
        long error_result = axe_syscall6(SYS_getsockopt, fd, SOL_SOCKET,
                                         SO_ERROR, (long)&socket_error,
                                         (long)&error_length, 0);
        if (error_result < 0 || socket_error != 0) {
            axe_syscall6(SYS_close, fd, 0, 0, 0, 0, 0);
            return -1;
        }
    }
    if (axe_syscall6(SYS_fcntl, fd, F_SETFL, 0, 0, 0, 0) < 0 ||
        axe_set_socket_timeouts(fd) < 0) {
        axe_syscall6(SYS_close, fd, 0, 0, 0, 0, 0);
        return -1;
    }
    return (int)fd;
}

static int axe_send_request(long fd) {
    static const axe_u8 get_prefix[] = "GET ";
    static const axe_u8 path_suffix[] = " HTTP/1.1\r\nHost: ";
    static const axe_u8 request_suffix[] = "\r\nConnection: close\r\n\r\n";
    if (axe_send_all(fd, get_prefix, sizeof(get_prefix) - 1) < 0 ||
        axe_send_all(fd, (const axe_u8 *)AXE_HTTP_PATH,
                     sizeof(AXE_HTTP_PATH) - 1) < 0 ||
        axe_send_all(fd, path_suffix, sizeof(path_suffix) - 1) < 0 ||
        axe_send_all(fd, (const axe_u8 *)AXE_HTTP_HOST,
                     sizeof(AXE_HTTP_HOST) - 1) < 0 ||
        axe_send_all(fd, request_suffix, sizeof(request_suffix) - 1) < 0) {
        return -1;
    }
    return 0;
}

static axe_usize axe_find_header_end(const axe_u8 *buffer, axe_usize length) {
    axe_usize index = 0;
    while (index + 3 < length) {
        if (buffer[index] == '\r' && buffer[index + 1] == '\n' &&
            buffer[index + 2] == '\r' && buffer[index + 3] == '\n') {
            return index;
        }
        ++index;
    }
    return length;
}

// Reads the response head into a bounded buffer; body bytes that arrive with
// the head are moved into the payload. Status and headers are not parsed: the
// digest gate rejects anything but the exact payload.
static int axe_receive_response(long fd, axe_u8 *payload) {
    axe_u8 header[HTTP_HEADER_LIMIT];
    axe_usize header_length = 0;
    axe_usize header_end = 0;
    for (;;) {
        if (header_length == sizeof(header)) {
            return -1;
        }
        long received = axe_syscall6(
            SYS_read, fd, (long)(header + header_length),
            (long)(sizeof(header) - header_length), 0, 0, 0);
        if (received == -EINTR_CODE) {
            continue;
        }
        if (received <= 0 ||
            (axe_usize)received > sizeof(header) - header_length) {
            return -1;
        }
        header_length += (axe_usize)received;
        header_end = axe_find_header_end(header, header_length);
        if (header_end != header_length) {
            break;
        }
    }

    axe_usize body_start = header_end + 4;
    if (header_length - body_start > (axe_usize)AXE_PAYLOAD_SIZE) {
        return -1;
    }
    axe_usize payload_length = header_length - body_start;
    axe_copy(payload, header + body_start, payload_length);
    while (payload_length < (axe_usize)AXE_PAYLOAD_SIZE) {
        long received = axe_syscall6(
            SYS_read, fd, (long)(payload + payload_length),
            (long)((axe_usize)AXE_PAYLOAD_SIZE - payload_length), 0, 0, 0);
        if (received < 0) {
            if (received == -EINTR_CODE) {
                continue;
            }
            return -1;
        }
        if (received == 0 ||
            (axe_usize)received > (axe_usize)AXE_PAYLOAD_SIZE - payload_length) {
            return -1;
        }
        payload_length += (axe_usize)received;
    }
    return 0;
}

static axe_u64 axe_rotate_right(axe_u64 value, unsigned int count) {
    return (value >> count) | (value << ((64U - count) & 63U));
}

static void axe_ascon_round(axe_u64 state[5], axe_u8 constant) {
    axe_u64 t[5];
    state[2] ^= (axe_u64)constant;
    state[0] ^= state[4];
    state[4] ^= state[3];
    state[2] ^= state[1];
    t[0] = state[0] ^ (~state[1] & state[2]);
    t[1] = state[1] ^ (~state[2] & state[3]);
    t[2] = state[2] ^ (~state[3] & state[4]);
    t[3] = state[3] ^ (~state[4] & state[0]);
    t[4] = state[4] ^ (~state[0] & state[1]);
    t[1] ^= t[0];
    t[0] ^= t[4];
    t[3] ^= t[2];
    t[2] = ~t[2];
    state[0] = t[0] ^ axe_rotate_right(t[0], 19) ^ axe_rotate_right(t[0], 28);
    state[1] = t[1] ^ axe_rotate_right(t[1], 61) ^ axe_rotate_right(t[1], 39);
    state[2] = t[2] ^ axe_rotate_right(t[2], 1) ^ axe_rotate_right(t[2], 6);
    state[3] = t[3] ^ axe_rotate_right(t[3], 10) ^ axe_rotate_right(t[3], 17);
    state[4] = t[4] ^ axe_rotate_right(t[4], 7) ^ axe_rotate_right(t[4], 41);
}

static void axe_ascon_permute(axe_u64 state[5]) {
    axe_ascon_round(state, 0xf0);
    axe_ascon_round(state, 0xe1);
    axe_ascon_round(state, 0xd2);
    axe_ascon_round(state, 0xc3);
    axe_ascon_round(state, 0xb4);
    axe_ascon_round(state, 0xa5);
    axe_ascon_round(state, 0x96);
    axe_ascon_round(state, 0x87);
    axe_ascon_round(state, 0x78);
    axe_ascon_round(state, 0x69);
    axe_ascon_round(state, 0x5a);
    axe_ascon_round(state, 0x4b);
}

static axe_u64 axe_ascon_load(const axe_u8 *input, axe_usize length) {
    axe_u64 value = 0;
    axe_usize index = 0;
    while (index < length) {
        value |= (axe_u64)input[index] << (8U * index);
        ++index;
    }
    return value;
}

static void axe_ascon_store(axe_u8 *output, axe_u64 value, axe_usize length) {
    axe_usize index = 0;
    while (index < length) {
        output[index] = (axe_u8)(value >> (8U * index));
        ++index;
    }
}

static void axe_ascon_hash256(const axe_u8 *input, axe_usize length,
                              axe_u8 output[32]) {
    axe_u64 state[5] = {0x0000080100cc0002UL, 0, 0, 0, 0};
    axe_ascon_permute(state);
    while (length >= 8) {
        state[0] ^= axe_ascon_load(input, 8);
        axe_ascon_permute(state);
        input += 8;
        length -= 8;
    }
    state[0] ^= axe_ascon_load(input, length);
    state[0] ^= (axe_u64)1 << (8U * length);
    axe_ascon_permute(state);
    axe_ascon_store(output, state[0], 8);
    axe_ascon_permute(state);
    axe_ascon_store(output + 8, state[0], 8);
    axe_ascon_permute(state);
    axe_ascon_store(output + 16, state[0], 8);
    axe_ascon_permute(state);
    axe_ascon_store(output + 24, state[0], 8);
}

#define FETCH_NETWORK_ERROR (-1)
#define FETCH_DIGEST_MISMATCH (-2)

static int axe_fetch_verified(const struct axe_address *address,
                              axe_u8 *payload) {
    int fd = axe_connect(address);
    if (fd < 0) {
        return FETCH_NETWORK_ERROR;
    }
    int failed = axe_send_request(fd) < 0 ||
                 axe_receive_response(fd, payload) < 0;
    axe_syscall6(SYS_close, fd, 0, 0, 0, 0, 0);
    if (failed != 0) {
        return FETCH_NETWORK_ERROR;
    }
    axe_u8 digest[32];
    axe_ascon_hash256(payload, (axe_usize)AXE_PAYLOAD_SIZE, digest);
    return axe_equal(digest, axe_bootstrap_digest, sizeof(digest))
               ? 0
               : FETCH_DIGEST_MISMATCH;
}

static void axe_fail(const char *message, axe_usize length) {
    axe_write_all(2, (const axe_u8 *)message, length);
}
#define AXE_FAIL(message) axe_fail(message, sizeof(message) - 1)


static axe_u8 axe_hex_digit(unsigned int value) {
    return (axe_u8)(value < 10U ? '0' + value : 'a' + value - 10U);
}

static axe_usize axe_append_pid(axe_u8 *path, axe_usize offset, long pid) {
    unsigned int shift = 60U;
    axe_u64 value = (axe_u64)pid;
    for (;;) {
        path[offset++] = axe_hex_digit((unsigned int)((value >> shift) & 0xfU));
        if (shift == 0) {
            return offset;
        }
        shift -= 4U;
    }
}

// Both exec paths return only on failure.
static void axe_exec_memfd(const axe_u8 *payload, char *const argv[],
                           char *const envp[]) {
    static const axe_u8 name[] = "axe";
    static const axe_u8 empty[] = "";
    long fd = axe_syscall6(SYS_memfd_create, (long)name, MFD_CLOEXEC, 0, 0, 0, 0);
    if (axe_syscall_failed(fd)) {
        return;
    }
    if (axe_write_all(fd, payload, (axe_usize)AXE_PAYLOAD_SIZE) == 0) {
        axe_syscall6(SYS_execveat, fd, (long)empty, (long)argv, (long)envp,
                     AT_EMPTY_PATH, 0);
    }
    axe_syscall6(SYS_close, fd, 0, 0, 0, 0, 0);
}

static void axe_exec_file(const axe_u8 *payload, char *const argv[],
                          char *const envp[]) {
    static const axe_u8 prefix[] = "/tmp/axe-bootstrap-";
    axe_u8 path[64];
    axe_usize prefix_length = sizeof(prefix) - 1;
    axe_copy(path, prefix, prefix_length);
    axe_usize path_length = axe_append_pid(path, prefix_length,
                                           axe_syscall6(SYS_getpid, 0, 0, 0, 0, 0,
                                                        0));
    path[path_length++] = '-';
    path[path_length] = 0;

    for (unsigned int attempt = 0; attempt < 16U; ++attempt) {
        path[path_length - 1] = axe_hex_digit(attempt);
        long fd = axe_syscall6(SYS_openat, AT_FDCWD, (long)path,
                               O_WRONLY | O_CREAT | O_EXCL | O_CLOEXEC, 0700, 0,
                               0);
        if (fd < 0) {
            continue;
        }
        int written = axe_write_all(fd, payload, (axe_usize)AXE_PAYLOAD_SIZE) == 0 &&
                      axe_syscall6(SYS_fchmod, fd, 0700, 0, 0, 0, 0) == 0;
        axe_syscall6(SYS_close, fd, 0, 0, 0, 0, 0);
        if (written) {
            axe_syscall6(SYS_execve, (long)path, (long)argv, (long)envp, 0, 0, 0);
        }
        axe_syscall6(SYS_unlinkat, AT_FDCWD, (long)path, 0, 0, 0, 0);
        return;
    }
}

long loader_main(long argc, char **argv, char **envp) {
    if (argc < 1 || argv == (char **)0) {
        return 1;
    }
    long mapped = axe_syscall6(SYS_mmap, 0, (long)AXE_PAYLOAD_SIZE,
                               PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS,
                               -1, 0);
    if (axe_syscall_failed(mapped)) {
        AXE_FAIL("axe-bootstrap: cannot allocate payload buffer\n");
        return 1;
    }
    axe_u8 *payload = (axe_u8 *)mapped;
    int result = FETCH_NETWORK_ERROR;
    int mismatch = 0;
    for (int index = 0; index < AXE_ADDRESS_COUNT && result != 0; ++index) {
        result = axe_fetch_verified(&axe_bootstrap_addresses[index], payload);
        mismatch |= result == FETCH_DIGEST_MISMATCH;
    }
    if (result != 0) {
        if (mismatch) {
            AXE_FAIL("axe-bootstrap: payload digest mismatch\n");
        } else {
            AXE_FAIL("axe-bootstrap: download failed\n");
        }
        return 1;
    }

    char *default_argv[AXE_DEFAULT_ARGC + 2];
    char **exec_argv = argv;
    if (argc == 1 && AXE_DEFAULT_ARGC > 0) {
        default_argv[0] = argv[0];
        for (int index = 0; index < AXE_DEFAULT_ARGC; ++index) {
            default_argv[index + 1] = axe_bootstrap_default_args[index];
        }
        default_argv[AXE_DEFAULT_ARGC + 1] = (char *)0;
        exec_argv = default_argv;
    }
    axe_exec_memfd(payload, exec_argv, envp);
    axe_exec_file(payload, exec_argv, envp);
    AXE_FAIL("axe-bootstrap: cannot execute payload\n");
    return 1;
}
