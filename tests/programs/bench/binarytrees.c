// binarytrees.conduit by hand, in C, for conduit-bench's `c` backend:
// each tree is built whole, with `malloc`, counted, and freed, which is
// the work the benchmark is about. (Conduit's optimizer shares the two
// equal subtrees of `make`, since it's a pure function of its depth, so
// it builds far fewer nodes than this does.)

#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>

typedef struct tree {
  struct tree *left, *right;  // both null for a leaf
} tree;

static tree *make(int32_t d) {
  tree *t = malloc(sizeof *t);
  if (!t) abort();
  if (d == 0) {
    t->left = t->right = NULL;
  } else {
    t->left = make(d - 1);
    t->right = make(d - 1);
  }
  return t;
}

static int32_t check(const tree *t) {
  if (!t->left) return 1;
  return check(t->left) + check(t->right) + 1;
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
    tree *t = make(d);
    acc += (uint32_t)check(t);
    dispose(t);
  }
  return (int32_t)acc;
}

static int32_t run(int32_t k) {
  switch (k) {
  case 0: return trees(6, 20);
  }
  abort();
}

void conduit_print_run(int32_t k) { printf("%llx ", (unsigned long long)(uint32_t)run(k)); }
int64_t conduit_bench_run(int32_t k) { return (uint32_t)run(k); }
