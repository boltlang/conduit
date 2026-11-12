//! cfold.conduit by hand, in Rust, for conduit-bench's `rust` backend: the
//! same expression tree, evaluated, folded in place, dropping the nodes
//! folding replaces, as reuse lets Conduit do, evaluated again, and
//! dropped. Integers wrap, as in Conduit.

enum Expr {
    Const(i32),
    // Its number, unused, so that nodes are as big as Conduit's.
    Var(#[allow(dead_code)] i32),
    Add(Box<Expr>, Box<Expr>),
    Mul(Box<Expr>, Box<Expr>),
}

fn build(n: i32, k: i32) -> Box<Expr> {
    if n == 0 {
        return Box::new(if k % 3 == 0 { Expr::Var(k) } else { Expr::Const(k) });
    }
    let k2 = k.wrapping_mul(2);
    let l = build(n - 1, k2);
    let r = build(n - 1, k2.wrapping_add(1));
    let prod = Box::new(Expr::Mul(Box::new(Expr::Const(k)), r));
    Box::new(Expr::Add(l, prod))
}

fn fold(e: &mut Expr) {
    let (add, l, r) = match e {
        Expr::Add(l, r) => (true, l, r),
        Expr::Mul(l, r) => (false, l, r),
        _ => return,
    };
    fold(l);
    fold(r);
    if let (Expr::Const(a), Expr::Const(b)) = (&**l, &**r) {
        let v = if add { a.wrapping_add(*b) } else { a.wrapping_mul(*b) };
        *e = Expr::Const(v);
    }
}

fn eval(e: &Expr) -> i32 {
    match e {
        Expr::Const(x) => *x,
        Expr::Var(_) => 1,
        Expr::Add(l, r) => eval(l).wrapping_add(eval(r)),
        Expr::Mul(l, r) => eval(l).wrapping_mul(eval(r)),
    }
}

fn cfold(n: i32) -> i32 {
    let mut e = build(n, 1);
    let before = eval(&e);
    fold(&mut e);
    let after = eval(&e);
    before.wrapping_sub(after)
}

fn run(k: i32) -> i32 {
    match k {
        0 => cfold(8),
        1 => cfold(18),
        _ => std::process::abort(),
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn conduit_print_run(k: i32) {
    use std::io::Write;
    print!("{:x} ", run(k) as u32);
    std::io::stdout().flush().unwrap();
}

#[unsafe(no_mangle)]
pub extern "C" fn conduit_bench_run(k: i32) -> i64 {
    run(k) as u32 as i64
}
