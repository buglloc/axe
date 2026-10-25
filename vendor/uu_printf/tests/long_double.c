/* Direct ABI/buffer tests for AXE's MIT-licensed long-double bridge. */
#define _GNU_SOURCE 1
/* Zig's optimized defaults must not disable the ABI checks. */
#undef NDEBUG
#include <assert.h>
#include <float.h>
#include <locale.h>
#include <stddef.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <wchar.h>

struct axe_printf_output {
    unsigned char *data;
    size_t length;
    size_t converted;
    size_t ignored;
    int issue;
};
int axe_printf_long_double(const char *, int, const char *, int, int,
                          unsigned char *, size_t, struct axe_printf_output *);
void axe_printf_free(void *);

static void expect(const char *operand, int supplied, const char *format,
                   int width, int precision, const char *expected, int issue)
{
    unsigned char scratch[7];
    struct axe_printf_output output = {0};
    locale_t before = uselocale((locale_t)0);
    int error = axe_printf_long_double(operand, supplied, format, width, precision,
                                      scratch, sizeof scratch, &output);
    assert(error == 0);
    assert(uselocale((locale_t)0) == before);
    assert(output.issue == issue);
    assert(output.length == strlen(expected));
    assert(memcmp(output.data, expected, output.length) == 0);
    if (output.data != scratch)
        axe_printf_free(output.data);
}

static int shift_jis_available(const char *name)
{
    locale_t locale = newlocale(LC_ALL_MASK, name, (locale_t)0);
    if (!locale)
        return 0;
    locale_t previous = uselocale(locale);
    if (!previous) {
        freelocale(locale);
        return 0;
    }
    mbstate_t state = {0};
    wchar_t character = 0;
    int available = MB_CUR_MAX > 1
        && mbrtowc(&character, "\xa6", 1, &state) == 1 && character == 0xff66;
    uselocale(previous);
    freelocale(locale);
    return available;
}

static void character_constants_in_shift_jis(void)
{
    const char *requested = getenv("AXE_TEST_SHIFT_JIS_LOCALE");
    const char *candidates[] = { "ja_JP.SJIS", "ja_JP.Shift_JIS", "ja_JP.SHIFT_JIS" };
    const char *selected = requested;
    if (requested) {
        assert(shift_jis_available(requested));
    } else {
        for (size_t i = 0; i < sizeof candidates / sizeof *candidates; ++i) {
            if (shift_jis_available(candidates[i])) {
                selected = candidates[i];
                break;
            }
        }
    }
    if (!selected) {
        fputs("SKIP: no installed Shift-JIS locale; set AXE_TEST_SHIFT_JIS_LOCALE to require it\n", stderr);
        return;
    }
    const char *old = getenv("LC_ALL");
    char *saved = old ? strdup(old) : NULL;
    assert(!old || saved);
    assert(setenv("LC_ALL", selected, 1) == 0);
    expect("\x27\xa6", 1, "%*.*Lf", 0, 0, "166", 0);
    expect("\x27\xa6Z", 1, "%*.*Lf", 0, 0, "65382", 0);
    if (saved)
        assert(setenv("LC_ALL", saved, 1) == 0);
    else
        assert(unsetenv("LC_ALL") == 0);
    free(saved);
}

int main(void)
{
    expect("1.255", 1, "%*.*Lf", 0, 2, "1.25", 0);
    expect("2.675", 1, "%*.*Lf", 0, 2, "2.67", 0);
    expect("2.5", 1, "%*.*Lf", 0, 0, "2", 0);
    expect("3.5", 1, "%*.*Lf", 0, 0, "4", 0);
    expect("1.25", 1, "%+0*.*Lf", 12, 2, "+00000001.25", 0);
    expect("1.25", 1, "%*.*Lf", -8, 2, "1.25    ", 0);
    expect("-0", 1, "%*.*Le", 0, 3, "-0.000e+00", 0);
    expect("12345.125", 1, "%*.*Lg", 0, 4, "1.235e+04", 0);
    expect("0.00125", 1, "%#*.*LG", 0, 4, "0.001250", 0);
    expect("0", 1, "%*.*La", 0, -1, "0x0p+0", 0);
#if LDBL_MANT_DIG == 64
    expect("1.5", 1, "%*.*La", 0, -1, "0xcp-3", 0);
    expect("0x1.fp+3", 1, "%*.*La", 0, 0, "0x1p+4", 0);
    expect("0x1p-16382", 1, "%*.*La", 0, -1, "0x8p-16385", 0);
    expect("0x1p-16445", 1, "%*.*La", 0, -1, "0x0.000000000000001p-16385", 0);
    expect("9007199254740993", 1, "%*.*Lf", 0, 0, "9007199254740993", 0);
#else
    expect("1.5", 1, "%*.*La", 0, -1, "0x1.8p+0", 0);
#endif
#if LDBL_MANT_DIG == 53
    expect("9007199254740993", 1, "%*.*Lf", 0, 0, "9007199254740992", 0);
#endif
#if LDBL_MANT_DIG == 113
    expect("9007199254740993", 1, "%*.*Lf", 0, 0, "9007199254740993", 0);
    expect("0x1p-16382", 1, "%*.*La", 0, -1, "0x1p-16382", 0);
    expect("0x1p-16494", 1, "%*.*La", 0, -1, "0x0.0000000000000000000000000001p-16382", 0);
#endif
    expect("1.25tail", 1, "%*.*Lf", 0, 2, "1.25", 3);
    expect("", 1, "%*.*Lf", 0, 2, "0.00", 1);
    expect("", 0, "%*.*Lf", 0, 2, "0.00", 0);
    expect("'A", 1, "%*.*Lf", 0, 2, "65.00", 0);
    expect("1e99999", 1, "%*.*Lg", 0, -1, "inf", 2);
    expect("1e-99999", 1, "%*.*Lg", 0, -1, "0", 2);
    character_constants_in_shift_jis();
    return 0;
}
