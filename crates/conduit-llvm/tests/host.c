// The test host for native programs, which `tests/programs.rs` links
// with each one: the functions of `conduit_test::eval::TestHost`, which
// `tests/programs/host/c.conduit` declares. Each is `conduit_test_` and
// its name, since C can't name a function `double`. The flat ones take
// what the flat ABI passes natively, which is the C ABI: an array as a
// pointer to its elements and its length, a list of byte buffers as
// `struct iovec`s, and results after the first through pointers.
//
// Integers wrap, as in the evaluator, so they're added as unsigned.

#include <stddef.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#include <sys/uio.h>

int32_t conduit_test_double(int32_t x) { return (int32_t)((uint32_t)x * 2); }

static uint32_t bytes(const int8_t *p, size_t n) {
  uint32_t sum = 0;
  for (size_t i = 0; i < n; i++) sum += (uint32_t)(int32_t)p[i];
  return sum;
}

int32_t conduit_test_sum(const int8_t *p, int32_t n) { return (int32_t)bytes(p, (size_t)n); }

void conduit_test_fill(int8_t *p, int32_t n, int32_t b) { memset(p, (uint8_t)b, (size_t)n); }

int32_t conduit_test_gather(const struct iovec *list, int32_t n, int32_t *total) {
  uint32_t sum = 0;
  for (int32_t i = 0; i < n; i++) sum += bytes(list[i].iov_base, list[i].iov_len);
  *total = (int32_t)sum;
  return n;
}

// `#[addr]`: the array's length doesn't come with it.
void conduit_test_squares(int32_t *p, int32_t n) {
  for (int32_t i = 0; i < n; i++) p[i] = i * i;
}

int32_t conduit_test_scale(int32_t x, int64_t *out) {
  *out = (int64_t)x * 1000000000000;
  return 0;
}

int32_t conduit_test_byte(uint8_t b) { return b; }

// Host values, as a native host gives them: its own pointer, and the
// functions that keep it alive and let go of it.
typedef struct conduit_host_ops {
  void (*retain)(void *value);
  void (*release)(void *value);
} conduit_host_ops;

typedef struct conduit_host {
  void *value;
  const conduit_host_ops *ops;
} conduit_host;

// A number, counted by the host, which frees it when the last reference
// goes. `live` counts those not freed, for the tests to check that Conduit
// releases each host value it gets exactly once.
typedef struct number {
  int32_t refs;
  int32_t n;
} number;

static int64_t live;

int64_t conduit_test_hosts_live(void) { return live; }

static void retain(void *value) { ((number *)value)->refs++; }

static void release(void *value) {
  number *x = value;
  if (--x->refs == 0) {
    free(x);
    live--;
  }
}

static const conduit_host_ops number_ops = {retain, release};

// The caller gets the only reference.
conduit_host conduit_test_wrap(int32_t n) {
  number *x = malloc(sizeof *x);
  if (!x) abort();
  x->refs = 1;
  x->n = n;
  live++;
  return (conduit_host){x, &number_ops};
}

// Borrowed for the call.
int32_t conduit_test_unwrap(void *value) { return ((number *)value)->n; }

// Null for a negative number.
conduit_host conduit_test_maybe(int32_t n) {
  if (n < 0) return (conduit_host){NULL, NULL};
  return conduit_test_wrap(n);
}

int32_t conduit_test_unwrap_or(void *value, int32_t d) {
  return value ? conduit_test_unwrap(value) : d;
}
