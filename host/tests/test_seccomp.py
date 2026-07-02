"""Runs INSIDE the sandbox to check whether the hardened seccomp profile blocks
dangerous SYS_ADMIN-gated syscalls (bpf, perf_event_open).

The hardened profile denies these via the profile's defaultAction, which returns
ENOSYS. Without the profile, --cap-add=SYS_ADMIN lets the default profile pass
them to the kernel (which then fails with EINVAL for our dummy args). So we treat
ENOSYS as "blocked by seccomp".
"""
import ctypes
import ctypes.util
import errno
import sys

# x86_64 syscall numbers.
SYS_PERF_EVENT_OPEN = 298
SYS_BPF = 321


def try_syscall(nr):
    libc = ctypes.CDLL(ctypes.util.find_library("c") or "libc.so.6", use_errno=True)
    ctypes.set_errno(0)
    libc.syscall(nr, 0, 0, 0, 0, 0)
    return ctypes.get_errno()


def is_blocked(nr):
    # ENOSYS (from the profile's defaultErrnoRet) or EPERM => denied by seccomp.
    return try_syscall(nr) in (errno.ENOSYS, errno.EPERM)


def main():
    print("=== Running Seccomp Hardening Test ===")
    bpf_blocked = is_blocked(SYS_BPF)
    perf_blocked = is_blocked(SYS_PERF_EVENT_OPEN)
    print(f"RESULT_BPF_BLOCKED={bpf_blocked}")
    print(f"RESULT_PERF_BLOCKED={perf_blocked}")

    try:
        with open("/output/seccomp_result.txt", "w") as f:
            f.write(f"bpf_blocked={bpf_blocked} perf_blocked={perf_blocked}")
    except Exception as e:
        print(f"WARN: could not write /output result: {e}", file=sys.stderr)


if __name__ == "__main__":
    main()
