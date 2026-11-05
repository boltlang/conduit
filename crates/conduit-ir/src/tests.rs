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
    let point = Ty::data(DataId(7), Mutability::Mut);
    assert_eq!(point.mutability(), Some(Mutability::Mut));
    assert_eq!(
        point.with_mutability(Mutability::Imm),
        Ty::data(DataId(7), Mutability::Imm)
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
fn at_most() {
    use Mutability::{Imm, Mut, Param, ReadOnly};
    let none = &[];
    assert!(Imm.at_most(ReadOnly, none));
    assert!(ReadOnly.at_most(Mut, none));
    assert!(!ReadOnly.at_most(Imm, none));
    assert!(!Mut.at_most(ReadOnly, none));
    // A parameter is imm or mut.
    assert!(Imm.at_most(Param(1), none));
    assert!(Param(1).at_most(Mut, none));
    assert!(Param(1).at_most(Param(1), none));
    assert!(!Param(1).at_most(Param(2), none));
    assert!(!Param(1).at_most(Imm, none));
    assert!(!ReadOnly.at_most(Param(1), none));
    // Assumptions chain.
    let chain = &[(Param(1), Param(2)), (Param(2), Imm)];
    assert!(Param(1).at_most(Param(2), chain));
    assert!(Param(1).at_most(Imm, chain));
    assert!(Param(2).at_most(Param(3), chain));
    // 'm2 is at most imm, so it's imm, which is at most anything.
    assert!(Param(2).at_most(Param(1), chain));
    // A cycle ends.
    let cycle = &[(Param(1), Param(2)), (Param(2), Param(1))];
    assert!(!Param(1).at_most(Param(3), cycle));
}

#[test]
fn effects() {
    let e = Effects::new;
    assert_eq!(Op::Prim(Prim::Add).effects(), Effects::PURE);
    assert_eq!(
        Op::Prim(Prim::Div(Sign::Signed)).effects(),
        e(Memory::None, true)
    );
    assert_eq!(Op::Prim(Prim::Rem(Sign::Signed)).effects(), Effects::PURE);
    assert_eq!(Op::ArrayClone.effects(), e(Memory::Reads, false));
    assert_eq!(Op::ArrayFreeze.effects(), e(Memory::Reads, false));
    assert_eq!(Op::ArrayGet.effects(), e(Memory::Reads, true));
    assert_eq!(Op::RefSet.effects(), e(Memory::Writes, false));
    assert_eq!(Op::ArraySet.effects(), e(Memory::Writes, true));
    assert_eq!(
        Op::Call(FuncId(0), vec![], vec![]).effects(),
        e(Memory::Unknown, true)
    );
    assert_eq!(
        Op::Closure(FuncId(0), vec![Mutability::Imm], vec![]).effects(),
        Effects::PURE
    );
    assert_eq!(
        Op::Perform(EffectId(0), 0).effects(),
        e(Memory::Unknown, true)
    );
    assert!(
        Op::Get(
            CtorId {
                data: DataId(0),
                index: 0
            },
            0
        )
        .effects()
        .is_removable()
    );
    assert!(!Op::ArrayGet.effects().is_removable());
}
