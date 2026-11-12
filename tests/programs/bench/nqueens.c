// nqueens.conduit by hand, in C, for conduit-bench's `c` backend: the
// same search, with the queens placed so far in a list whose cells are on
// the stack, since none outlives the call that makes it.

#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>

typedef struct list {
  int32_t head;
  const struct list *tail;
} list;

static int32_t safe(int32_t q, int32_t d, const list *qs) {
  for (; qs; qs = qs->tail, d++) {
    int32_t h = qs->head;
    if (q == h || q == h + d || q == h - d) return 0;
  }
  return 1;
}

static int32_t place(int32_t n, int32_t row, const list *qs) {
  if (row == n) return 1;
  int32_t acc = 0;
  for (int32_t c = 0; c < n; c++) {
    if (safe(c, 1, qs)) {
      list cell = {c, qs};
      acc += place(n, row + 1, &cell);
    }
  }
  return acc;
}

static int32_t run(int32_t k) {
  switch (k) {
  case 0: return place(6, 0, NULL);
  case 1: return place(8, 0, NULL);
  }
  abort();
}

void conduit_print_run(int32_t k) { printf("%llx ", (unsigned long long)(uint32_t)run(k)); }
int64_t conduit_bench_run(int32_t k) { return (uint32_t)run(k); }
