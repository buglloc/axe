#ifndef AXE_BOOTSTRAP_TYPES_H
#define AXE_BOOTSTRAP_TYPES_H

typedef unsigned char axe_u8;
typedef unsigned short axe_u16;
typedef unsigned int axe_u32;
typedef unsigned long axe_u64;
typedef long axe_s64;
typedef __SIZE_TYPE__ axe_usize;

_Static_assert(sizeof(axe_u8) == 1, "u8 width");
_Static_assert(sizeof(axe_u16) == 2, "u16 width");
_Static_assert(sizeof(axe_u32) == 4, "u32 width");
_Static_assert(sizeof(axe_u64) == 8, "u64 width");
_Static_assert(sizeof(axe_usize) == 8, "size width");

#endif
