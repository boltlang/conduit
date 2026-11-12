// binarytrees.conduit by hand, in C, for conduit-bench's `c` backend: the
// same trees, each built whole with `malloc`, checked, and freed.
// Integers wrap, as in Conduit.

#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>

typedef struct tree {
  struct tree *left, *right;  // both null for a leaf
  int32_t item;
} tree;

static tree *make(int32_t item, int32_t d) {
  tree *t = malloc(sizeof *t);
  if (!t) abort();
  t->item = item;
  if (d == 0) {
    t->left = t->right = NULL;
  } else {
    int32_t i2 = (int32_t)((uint32_t)item * 2);
    t->left = make((int32_t)((uint32_t)i2 - 1), d - 1);
    t->right = make(i2, d - 1);
  }
  return t;
}

static uint32_t check(const tree *t) {
  if (!t->left) return (uint32_t)t->item;
  return (uint32_t)t->item + check(t->left) - check(t->right);
}

static void dispose(tree *t) {
  if (t->left) {
    dispose(t->left);
    dispose(t->right);
  }
  free(t);
}

static int32_t trees(int32_t d, int32_t n) {
  uint32_t acc = 0;
  for (int32_t i = n; i != 0; i--) {
    tree *t = make(i, d);
    acc += check(t);
    dispose(t);
  }
  return (int32_t)acc;
}

static int32_t run(int32_t k) {
  switch (k) {
  case 0: return trees(6, 20);
  case 1: return trees(16, 20);
  }
  abort();
}

void conduit_print_run(int32_t k) { printf("%llx ", (unsigned long long)(uint32_t)run(k)); }
int64_t conduit_bench_run(int32_t k) { return (uint32_t)run(k); }
