// cfold.conduit by hand, in C, for conduit-bench's `c` backend: the same
// expression tree, evaluated, folded in place, freeing the nodes folding
// replaces, as reuse lets Conduit do, evaluated again, and freed. Integers
// wrap, as in Conduit.

#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>

enum { CONST, VAR, ADD, MUL };

typedef struct expr {
  int32_t tag;
  int32_t value;  // CONST and VAR
  struct expr *left, *right;  // ADD and MUL
} expr;

static expr *node(int32_t tag, int32_t value, expr *l, expr *r) {
  expr *e = malloc(sizeof *e);
  if (!e) abort();
  e->tag = tag;
  e->value = value;
  e->left = l;
  e->right = r;
  return e;
}

static expr *build(int32_t n, int32_t k) {
  if (n == 0) return node(k % 3 == 0 ? VAR : CONST, k, NULL, NULL);
  int32_t k2 = (int32_t)((uint32_t)k * 2);
  expr *l = build(n - 1, k2);
  expr *r = build(n - 1, (int32_t)((uint32_t)k2 + 1));
  expr *prod = node(MUL, 0, node(CONST, k, NULL, NULL), r);
  return node(ADD, 0, l, prod);
}

static void fold(expr *e) {
  if (e->tag != ADD && e->tag != MUL) return;
  fold(e->left);
  fold(e->right);
  if (e->left->tag == CONST && e->right->tag == CONST) {
    uint32_t a = (uint32_t)e->left->value, b = (uint32_t)e->right->value;
    e->value = (int32_t)(e->tag == ADD ? a + b : a * b);
    e->tag = CONST;
    free(e->left);
    free(e->right);
    e->left = e->right = NULL;
  }
}

static uint32_t eval(const expr *e) {
  switch (e->tag) {
  case CONST: return (uint32_t)e->value;
  case VAR: return 1;
  case ADD: return eval(e->left) + eval(e->right);
  default: return eval(e->left) * eval(e->right);
  }
}

static void dispose(expr *e) {
  if (e->tag == ADD || e->tag == MUL) {
    dispose(e->left);
    dispose(e->right);
  }
  free(e);
}

static int32_t cfold(int32_t n) {
  expr *e = build(n, 1);
  uint32_t before = eval(e);
  fold(e);
  uint32_t after = eval(e);
  dispose(e);
  return (int32_t)(before - after);
}

static int32_t run(int32_t k) {
  switch (k) {
  case 0: return cfold(8);
  }
  abort();
}

void conduit_print_run(int32_t k) { printf("%llx ", (unsigned long long)(uint32_t)run(k)); }
int64_t conduit_bench_run(int32_t k) { return (uint32_t)run(k); }
