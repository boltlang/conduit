use crate::*;

fn var(v: Var) -> Atom {
    Atom::Var(v)
}

fn int(n: i32) -> Atom {
    Atom::Lit(Lit::I32(n))
}

fn prim(binds: Vec<Var>, p: Prim, args: Vec<Atom>, body: Block) -> Block {
    (body).with_let(Let {
        binds,
        op: Op::Prim(p),
        args,
        source: None,
    })
}

fn jump(target: Label, args: Vec<Atom>) -> Block {
    Block::from(Tail::Jump {
        target,
        args,
        source: None,
    })
}

/// `sum` from the design doc: a loop with two mutable locals, as a
/// recursive join point.
#[test]
fn sum_loop() {
    let mut f = Func::new(vec![Ty::I32], vec![Ty::I32]);
    let n = f.params[0];
    let lp = f.new_label();
    let i = f.new_var(Ty::I32);
    let acc = f.new_var(Ty::I32);
    let done = f.new_var(Ty::I32);
    let i2 = f.new_var(Ty::I32);
    let acc2 = f.new_var(Ty::I32);

    let step = prim(
        vec![i2],
        Prim::Add,
        vec![var(i), int(1)],
        prim(
            vec![acc2],
            Prim::Add,
            vec![var(acc), var(i)],
            jump(lp, vec![var(i2), var(acc2)]),
        ),
    );
    let loop_body = prim(
        vec![done],
        Prim::Ge(Sign::Signed),
        vec![var(i), var(n)],
        Block::from(Tail::Case {
            scrut: var(done),
            arms: vec![Arm {
                tag: Tag::Int(1),
                body: jump(Label::RET, vec![var(acc)]),
            }],
            default: Some(Box::new(step)),
        }),
    );
    f.body = Block::from(Tail::Join {
        rec: true,
        defs: vec![JoinDef {
            label: lp,
            params: vec![i, acc],
            body: loop_body,
        }],
        body: Box::new(jump(lp, vec![int(0), int(0)])),
    });

    assert_eq!(f.labels, 2);
    assert_eq!(f.var_ty(acc2), &Ty::I32);
    assert_eq!(f.clone(), f);
}

#[test]
fn mutability() {
    let point = Ty::Data(DataId(7), Mutability::Mut);
    assert_eq!(point.mutability(), Some(Mutability::Mut));
    assert_eq!(
        point.with_mutability(Mutability::Imm),
        Ty::Data(DataId(7), Mutability::Imm)
    );
    assert_eq!(Ty::I32.mutability(), None);
    assert_eq!(
        Ty::Ref(Box::new(Ty::I32)).mutability(),
        Some(Mutability::Mut)
    );

    assert!(Mutability::Imm.is_sub(Mutability::ReadOnly));
    assert!(Mutability::Mut.is_sub(Mutability::ReadOnly));
    assert!(!Mutability::Mut.is_sub(Mutability::Imm));
    assert!(!Mutability::ReadOnly.is_sub(Mutability::Mut));
}

#[test]
fn effects() {
    assert_eq!(Op::Prim(Prim::Add).effects(), Effects::Pure);
    assert_eq!(
        Op::Prim(Prim::Div(Sign::Signed)).effects(),
        Effects::Unknown
    );
    assert_eq!(Op::Copy(Mutability::Imm).effects(), Effects::Reads);
    assert_eq!(Op::Freeze.effects(), Effects::Reads);
    assert_eq!(Op::RefSet.effects(), Effects::Writes);
    assert_eq!(Op::Perform(EffectId(0), 0).effects(), Effects::Unknown);
}
