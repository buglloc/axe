/* AXE's uutils printf patch; distributed under this package's MIT license. */
#define _GNU_SOURCE 1
#include <errno.h>
#include <fenv.h>
#include <float.h>
#include <limits.h>
#include <locale.h>
#include <math.h>
#include <stddef.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <wchar.h>
#ifdef __APPLE__
#include <xlocale.h>
#endif

struct axe_printf_output {
    unsigned char *data;
    size_t length;
    size_t converted;
    size_t ignored;
    int issue;
};

struct buffer {
    unsigned char *data;
    size_t capacity;
    size_t length;
};

static int append(struct buffer *out, const char *bytes, size_t count)
{
    if (count > (size_t)INT_MAX - out->length) {
        errno = EOVERFLOW;
        return -1;
    }
    size_t available = out->length < out->capacity - 1
        ? out->capacity - 1 - out->length : 0;
    size_t copied = count < available ? count : available;
    if (copied)
        memcpy(out->data + out->length, bytes, copied);
    out->length += count;
    return 0;
}

static int repeat(struct buffer *out, unsigned char byte, size_t count)
{
    if (count > (size_t)INT_MAX - out->length) {
        errno = EOVERFLOW;
        return -1;
    }
    size_t available = out->length < out->capacity - 1
        ? out->capacity - 1 - out->length : 0;
    size_t copied = count < available ? count : available;
    if (copied)
        memset(out->data + out->length, byte, copied);
    out->length += count;
    return 0;
}

/* GNU's hexadecimal significand uses the target long-double mantissa nibble,
 * including on musl, whose %La chooses a different normalization. Decimal
 * conversions go through the target libc without narrowing to double. */
static int hexadecimal(unsigned char *data, size_t capacity, const char *format,
                       int width, int precision, long double value)
{
    _Static_assert(LDBL_MANT_DIG <= 128, "unsupported long double mantissa");
    unsigned char digits[(LDBL_MANT_DIG + 3) / 4];
    const int leading_bits = (LDBL_MANT_DIG - 1) % 4 + 1;
    const int fractional_digits = (LDBL_MANT_DIG - leading_bits + 3) / 4;
    int negative = signbit(value) != 0;
    int upper = strchr(format, 'A') != NULL;
    int left = strchr(format, '-') != NULL || width < 0;
    int zero_pad = strchr(format, '0') != NULL && !left;
    int alternate = strchr(format, '#') != NULL;
    char sign = negative ? '-' : strchr(format, '+') ? '+' : strchr(format, ' ') ? ' ' : 0;
    const char *alphabet = upper ? "0123456789ABCDEF" : "0123456789abcdef";
    const char *radix = localeconv()->decimal_point;
    size_t radix_length = strlen(radix);
    int exponent = 0;
    int leading = 0;
    int count = 0;
    value = fabsl(value);
    if (value != 0) {
        (void)frexpl(value, &exponent);
        exponent -= leading_bits;
        if (exponent < LDBL_MIN_EXP - leading_bits)
            exponent = LDBL_MIN_EXP - leading_bits;
        long double fraction = scalbnl(value, -exponent);
        leading = (int)fraction;
        fraction -= leading;
        for (int i = 0; i < fractional_digits; ++i) {
            fraction *= 16;
            digits[count++] = (unsigned char)fraction;
            fraction -= digits[count - 1];
        }
    }
    if (precision >= 0 && precision < count) {
        int next = digits[precision];
        int remaining = (next & 7) != 0;
        for (int i = precision + 1; i < count; ++i)
            remaining |= digits[i] != 0;
        int discarded = next != 0 || remaining;
        int last = precision ? digits[precision - 1] : leading;
        int round_up;
        switch (fegetround()) {
        case FE_UPWARD: round_up = !negative && discarded; break;
        case FE_DOWNWARD: round_up = negative && discarded; break;
        case FE_TOWARDZERO: round_up = 0; break;
        default: round_up = next > 8 || (next == 8 && (remaining || (last & 1))); break;
        }
        count = precision;
        if (round_up) {
            int i = count;
            while (i > 0 && digits[i - 1] == 15)
                digits[--i] = 0;
            if (i > 0)
                ++digits[i - 1];
            else if (++leading == 16) {
                leading = 1;
                exponent += 4;
            }
        }
    }
    if (precision < 0) {
        while (count > 0 && digits[count - 1] == 0)
            --count;
        precision = count;
    }
    char exponent_text[32];
    int exponent_length = snprintf(exponent_text, sizeof exponent_text, "%c%+d", upper ? 'P' : 'p', exponent);
    if (exponent_length < 0 || (size_t)exponent_length >= sizeof exponent_text) {
        errno = EOVERFLOW;
        return -1;
    }
    size_t point_length = precision || alternate ? radix_length : 0;
    size_t length = (sign != 0) + 3u + point_length + (size_t)precision + (size_t)exponent_length;
    if (length > INT_MAX) {
        errno = EOVERFLOW;
        return -1;
    }
    size_t field_width = width < 0 ? (size_t)(-(int64_t)width) : (size_t)width;
    size_t padding = field_width > length ? field_width - length : 0;
    struct buffer out = { data, capacity, 0 };
    if (!left && !zero_pad && repeat(&out, ' ', padding) < 0) return -1;
    if (sign && append(&out, &sign, 1) < 0) return -1;
    if (append(&out, upper ? "0X" : "0x", 2) < 0) return -1;
    if (zero_pad && repeat(&out, '0', padding) < 0) return -1;
    char digit = alphabet[leading];
    if (append(&out, &digit, 1) < 0) return -1;
    if (point_length && append(&out, radix, point_length) < 0) return -1;
    int printed = count < precision ? count : precision;
    for (int i = 0; i < printed; ++i) {
        digit = alphabet[digits[i]];
        if (append(&out, &digit, 1) < 0) return -1;
    }
    if (repeat(&out, '0', (size_t)(precision - printed)) < 0) return -1;
    if (append(&out, exponent_text, (size_t)exponent_length) < 0) return -1;
    if (left && repeat(&out, ' ', padding) < 0) return -1;
    data[out.length < capacity ? out.length : capacity - 1] = 0;
    return (int)out.length;
}

static int render(unsigned char *data, size_t capacity, const char *format,
                  int width, int precision, long double value)
{
    if ((strchr(format, 'a') || strchr(format, 'A')) && isfinite(value))
        return hexadecimal(data, capacity, format, width, precision, value);
    return snprintf((char *)data, capacity, format, width, precision, value);
}

/* All strings/buffers are borrowed for this call. On success data points into
 * scratch or to malloc storage released by axe_printf_free; length excludes
 * NUL. On failure no allocation escapes. No long double crosses the C ABI. */
int axe_printf_long_double(const char *operand, int supplied, const char *format,
                          int width, int precision, unsigned char *scratch,
                          size_t capacity, struct axe_printf_output *output)
{
    int error = 0;
    unsigned char *allocation = NULL;
    locale_t selected = newlocale(LC_ALL_MASK, "", (locale_t)0);
    if (!selected)
        selected = newlocale(LC_ALL_MASK, "C", (locale_t)0);
    if (!selected)
        return errno ? errno : ENOMEM;
    locale_t previous = uselocale(selected);
    if (!previous) {
        error = errno ? errno : EINVAL;
        freelocale(selected);
        return error;
    }
    long double value = 0;
    output->issue = 0;
    output->ignored = 0;
    output->converted = 0;
    if (supplied && (operand[0] == '\'' || operand[0] == '"') && operand[1]) {
        size_t consumed = 1;
        value = (unsigned char)operand[1];
        if (MB_CUR_MAX > 1 && operand[2]) {
            mbstate_t state = {0};
            wchar_t character;
            size_t bytes = mbrtowc(&character, operand + 1, strlen(operand + 1), &state);
            if (bytes != (size_t)-1 && bytes != (size_t)-2 && bytes != 0) {
                value = character;
                consumed = bytes;
            }
        }
        if (operand[consumed + 1])
            output->ignored = consumed + 1;
    } else if (supplied) {
        char *end;
        errno = 0;
        value = strtold(operand, &end);
        int range_error = errno == ERANGE;
        /* GNU accepts the C decimal point even in a non-C numeric locale. */
        locale_t plain = newlocale(LC_ALL_MASK, "C", (locale_t)0);
        if (!plain) {
            error = errno ? errno : ENOMEM;
            goto cleanup;
        }
        char *plain_end;
        errno = 0;
        long double plain_value = strtold_l(operand, &plain_end, plain);
        int plain_range = errno == ERANGE;
        freelocale(plain);
        if (plain_end > end) {
            value = plain_value;
            end = plain_end;
            range_error = plain_range;
        }
        output->converted = (size_t)(end - operand);
        output->issue = end == operand ? 1 : range_error ? 2 : *end ? 3 : 0;
    }
    errno = 0;
    int needed = render(scratch, capacity, format, width, precision, value);
    if (needed < 0) {
        error = errno ? errno : EOVERFLOW;
        goto cleanup;
    }
    if ((size_t)needed >= capacity) {
        if ((size_t)needed == SIZE_MAX) {
            error = EOVERFLOW;
            goto cleanup;
        }
        allocation = malloc((size_t)needed + 1);
        if (!allocation) {
            error = ENOMEM;
            goto cleanup;
        }
        int written = render(allocation, (size_t)needed + 1, format, width, precision, value);
        if (written != needed) {
            error = errno ? errno : EIO;
            goto cleanup;
        }
    }
    output->data = allocation ? allocation : scratch;
    output->length = (size_t)needed;
    allocation = NULL;
cleanup:
    free(allocation);
    uselocale(previous);
    freelocale(selected);
    return error;
}

void axe_printf_free(void *data)
{
    free(data);
}
