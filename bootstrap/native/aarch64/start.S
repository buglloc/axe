.section .text.start,"ax",@progbits
.global _start
.type _start,@function
_start:
    mov x29, xzr
    ldr x0, [sp]
    add x1, sp, #8
    add x2, x1, x0, lsl #3
    add x2, x2, #8
    bl loader_main
    mov x8, #93
    svc #0
    b .
.size _start, .-_start
