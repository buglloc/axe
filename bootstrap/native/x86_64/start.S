.section .text.start,"ax",@progbits
.global _start
.type _start,@function
_start:
    xorq %rbp, %rbp
    movq (%rsp), %rdi
    leaq 8(%rsp), %rsi
    leaq 8(%rsi,%rdi,8), %rdx
    andq $-16, %rsp
    call loader_main
    movq %rax, %rdi
    movq $60, %rax
    syscall
    hlt
.size _start, .-_start
