// bstinsert.conduit by hand, in C, for conduit-bench's `c` backend: the
// same keys inserted into an unbalanced search tree, which C changes in
// place, as reuse makes Conduit's copy of each path, and frees at the end.

#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>

typedef struct tree {
  struct tree *left, *right;
  int32_t key;
} tree;

static void insert(tree **at, int32_t k) {
  while (*at) {
    tree *t = *at;
    if (k < t->key) at = &t->left;
    else if (k > t->key) at = &t->right;
    else return;
  }
  tree *t = malloc(sizeof *t);
  if (!t) abort();
  t->left = t->right = NULL;
  t->key = k;
  *at = t;
}

static uint32_t sum(const tree *t) {
  if (!t) return 0;
  return sum(t->left) + sum(t->right) + (uint32_t)t->key;
}

static void dispose(tree *t) {
  if (!t) return;
  dispose(t->left);
  dispose(t->right);
  free(t);
}

static int32_t bst(int32_t n) {
  tree *t = NULL;
  for (int32_t i = 0; i != n; i++) insert(&t, (int32_t)((uint32_t)i * 37) % 101);
  int32_t s = (int32_t)sum(t);
  dispose(t);
  return s;
}

static int32_t run(int32_t k) {
  switch (k) {
  case 0: return bst(200);
  }
  abort();
}

void conduit_print_run(int32_t k) { printf("%llx ", (unsigned long long)(uint32_t)run(k)); }
int64_t conduit_bench_run(int32_t k) { return (uint32_t)run(k); }
