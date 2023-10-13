//! Python terms
use circ::cfg::cfg;
use circ::circify::{CirCtx, Embeddable, Typed};
use crate::front::field_list::FieldList;
use circ::ir::opt::cfold::fold as constant_fold;
use circ::ir::term::*;
use circ::term;

use rug::Integer;

use std::collections::BTreeMap;
use std::fmt::{self, Display, Formatter};

// Phase 1: Mapping language values to CirC-IR

#[derive(Clone, PartialEq, Eq)]
pub enum Ty {
    Field,
    Bool,
    // To handle signed ints we probably need integer promotion
    Uint(usize),
    DataClass(String, FieldList<Ty>),
    Array(usize, Box<Ty>),
    MutArray(usize),
    // could we support other mutable types
    // like dicts, or other PyTypes
}

impl Display for Ty {
    fn fmt(&self, f: &mut Formatter) -> fmt::Result {
        match self {
            Ty::Field => write!(f, "field"),
            Ty::Bool => write!(f, "bool"),
            Ty::Uint(w) => write!(f, "s{w}"),
            Ty::DataClass(n, fields) => {
                let mut o = f.debug_struct(n);
                for (f_name, f_ty) in fields.fields() {
                    o.field(f_name, f_ty);
                }
                o.finish()
            }
            Ty::Array(n, b) => {
                let mut dims = vec![n];
                let mut bb = b.as_ref();
                while let Ty::Array(n, b) = bb {
                    bb = b.as_ref();
                    dims.push(n);
                }
                write!(f, "{bb}")?;
                dims.iter().try_for_each(|d| write!(f, "[{d}]"))
            }
            Ty::MutArray(n) => write!(f, "MutArray({n})"),
        }
    }
}

impl fmt::Debug for Ty {
    fn fmt(&self, f: &mut Formatter) -> fmt::Result {
        write!(f, "{self}")
    }
}

pub fn default_field() -> circ_fields::FieldT {
    cfg().field().clone()
}

fn default_field_sort() -> Sort {
    Sort::Field(default_field())
}

impl Ty {
    fn sort(&self) -> Sort {
        match self {
            Self::Field => default_field_sort(),
            Self::Bool => Sort::Bool,
            Self::Uint(w) => Sort::BitVector(*w),
            Self::DataClass(_name, fs) => {
                Sort::Tuple(fs.fields().map(|(_f_name, f_ty)| f_ty.sort()).collect())
            }
            Self::Array(n, b) => {
                Sort::Array(Box::new(default_field_sort()), Box::new(b.sort()), *n)
            }
            Self::MutArray(n) => Sort::Array(
                Box::new(default_field_sort()),
                Box::new(default_field_sort()),
                *n,
            ),
        }
    }
    
    fn default_ir_term(&self) -> Term {
        self.sort().default_term()
    }
    
    pub fn default(&self) -> PyTerm {
        PyTerm {
            ty: self.clone(),
            term: self.default_ir_term(),
        }
    }
    
    /// New class type, sorting the keys.
    pub fn new_class<I: IntoIterator<Item = (String, Ty)>>(name: String, fields: I) -> Self {
        Self::DataClass(name, FieldList::new(fields.into_iter().collect()))
    }
    
    /// Array value type
    pub fn array_val_ty(&self) -> &Self {
        match self {
            Self::Array(_, b) => b,
            _ => panic!("Not an array type: {:?}", self),
        }
    }
}

#[derive(Clone, Debug)]
pub struct PyTerm {
    pub ty: Ty,
    pub term: Term,
}

impl PyTerm {
    pub fn new(ty: Ty, term: Term) -> Self {
        Self { ty, term }
    }
    
    pub fn type_(&self) -> &Ty {
        &self.ty
    }
    
    // Get all IR terms inside this value, as a list
    pub fn terms(&self) -> Vec<Term> {
        let mut output: Vec<Term> = Vec::new();
        fn terms_tail(term: &Term, output: &mut Vec<Term>) {
            match check(term) {
                Sort::Bool | Sort::BitVector(_) | Sort::Field(_) => output.push(term.clone()),
                Sort::Array(_k, _v, size) => {
                    for i in 0..size {
                        terms_tail(&term![Op::Select; term.clone(), pf_lit_ir(i)], output)
                    }
                }
                Sort::Tuple(sorts) => {
                    for i in 0..sorts.len() {
                        terms_tail(&term![Op::Field(i); term.clone()], output)
                    }
                }
                s => unreachable!("Unreachable IR sort {} in ZoK", s),
            }
        }
        terms_tail(&self.term, &mut output);
        output
    }
    
    fn unwrap_array_ir(self) -> Result<Vec<Term>, String> {
        match &self.ty {
            Ty::Array(size, _sort) => Ok((0..*size)
                .map(|i| term![Op::Select; self.term.clone(), pf_lit_ir(i)])
                .collect()),
            Ty::MutArray(size) => Ok((0..*size)
                .map(|i| term![Op::Select; self.term.clone(), pf_lit_ir(i)])
                .collect()),
                s => Err(format!("Not an array: {s}")),
        }
    }
    
    pub fn unwrap_array(self) -> Result<Vec<PyTerm>, String> {
        match &self.ty {
            Ty::Array(_size, sort) => {
                let sort = (**sort).clone();
                Ok(self
                    .unwrap_array_ir()?
                    .into_iter()
                    .map(|t| PyTerm::new(sort.clone(), t))
                    .collect())
            }
            Ty::MutArray(_size) => Ok(self
                .unwrap_array_ir()?
                .into_iter()
                .map(|t| PyTerm::new(Ty::Field, t))
                .collect()),
            s => Err(format!("Not an array: {s}")),
        }
    }
    
    pub fn new_array(v: Vec<PyTerm>) -> Result<PyTerm, String> {
        array(v)
    }
    
    pub fn new_class(name: String, fields: Vec<(String, PyTerm)>) -> PyTerm {
        let (field_tys, ir_terms): (Vec<_>, Vec<_>) = fields
            .into_iter()
            .map(|(name, t)| ((name.clone(), t.ty), (name, t.term)))
            .unzip();
        let field_ty_list = FieldList::new(field_tys);
        let ir_term = term(Op::Tuple, {
            let with_indices: BTreeMap<usize, Term> = ir_terms
                .into_iter()
                .map(|(name, t)| (field_ty_list.search(&name).unwrap().0, t))
                .collect();
            with_indices.into_values().collect()
        });
        PyTerm::new(Ty::DataClass(name, field_ty_list), ir_term)
    }
    
    pub fn new_field<I>(v: I) -> Self
    where
        Integer: From<I>,
    {
        PyTerm::new(Ty::Field, pf_lit_ir(v))
    }
    
    pub fn new_u8<I>(v: I) -> Self
    where
        Integer: From<I>,
    {
        PyTerm::new(Ty::Uint(16), bv_lit(v, 16))
    }
    
    pub fn new_u16<I>(v: I) -> Self
    where
        Integer: From<I>,
    {
        PyTerm::new(Ty::Uint(16), bv_lit(v, 16))
    }

    pub fn new_u32<I>(v: I) -> Self
    where
        Integer: From<I>,
    {
        PyTerm::new(Ty::Uint(32), bv_lit(v, 32))
    }

    pub fn new_u64<I>(v: I) -> Self
    where
        Integer: From<I>,
    {
        PyTerm::new(Ty::Uint(64), bv_lit(v, 64))
    }
    
    pub fn pretty<W: std::io::Write>(&self, f: &mut W) -> Result<(), std::io::Error> {
        use std::io::{Error, ErrorKind};
        let val = match &self.term.op() {
            Op::Const(v) => Ok(v),
            _ => Err(Error::new(ErrorKind::Other, "not a const val")),
        }?;
        match val {
            Value::Bool(b) => write!(f, "{b}"),
            Value::Field(fe) => write!(f, "{}f", fe.i()),
            Value::BitVector(bv) => match bv.width() {
                8 => write!(f, "0x{:02x}", bv.uint()),
                16 => write!(f, "0x{:04x}", bv.uint()),
                32 => write!(f, "0x{:08x}", bv.uint()),
                64 => write!(f, "0x{:016x}", bv.uint()),
                _ => unreachable!(),
            },
            Value::Tuple(vs) => {
                let (n, fl) = if let Ty::DataClass(n, fl) = &self.ty {
                    Ok((n, fl))
                } else {
                    Err(Error::new(
                        ErrorKind::Other,
                        "expected dataclass, got something else",
                    ))
                }?;
                write!(f, "{n} {{ ")?;
                fl.fields().zip(vs.iter()).try_for_each(|((n, ty), v)| {
                    write!(f, "{n}: ")?;
                    PyTerm::new(ty.clone(), leaf_term(Op::Const(v.clone()))).pretty(f)?;
                    write!(f, ", ")
                })?;
                write!(f, "}}")
            }
            Value::Array(arr) => {
                let inner_ty = if let Ty::Array(_, ty) = &self.ty {
                    Ok(ty)
                } else {
                    Err(Error::new(
                        ErrorKind::Other,
                        "expected array, got something else",
                    ))
                }?;
                write!(f, "[")?;
                arr.key_sort
                    .elems_iter()
                    .take(arr.size)
                    .try_for_each(|idx| {
                        PyTerm::new(
                            *inner_ty.clone(),
                            leaf_term(Op::Const(arr.select(idx.as_value_opt().unwrap()))),
                        )
                        .pretty(f)?;
                        write!(f, ", ")
                    })?;
                write!(f, "]")
            }
            _ => unreachable!(),
        }
    }
}

impl Display for PyTerm {
    fn fmt(&self, f: &mut Formatter) -> fmt::Result {
        write!(f, "{}", self.term)
    }
}

// Phase 2: Hooking into Circify

fn wrap_bin_op(
    name: &str,
    fu: Option<fn(Term, Term) -> Term>,
    ff: Option<fn(Term, Term) -> Term>,
    fb: Option<fn(Term, Term) -> Term>,
    a: PyTerm,
    b: PyTerm,
) -> Result<PyTerm, String> {
    match (&a.ty, &b.ty, fu, ff, fb) {
        (Ty::Uint(na), Ty::Uint(nb), Some(fu), _, _) if na == nb => {
            Ok(PyTerm::new(Ty::Uint(*na), fu(a.term.clone(), b.term.clone())))
        }
        (Ty::Bool, Ty::Bool, _, _, Some(fb)) => {
            Ok(PyTerm::new(Ty::Bool, fb(a.term.clone(), b.term.clone())))
        }
        (Ty::Field, Ty::Field, _, Some(ff), _) => {
            Ok(PyTerm::new(Ty::Field, ff(a.term.clone(), b.term.clone())))
        }
        (x, y, _, _, _) => Err(format!("Cannot perform op '{name}' on {x} and {y}")),
    }
}

fn wrap_bin_pred(
    name: &str,
    fu: Option<fn(Term, Term) -> Term>,
    ff: Option<fn(Term, Term) -> Term>,
    fb: Option<fn(Term, Term) -> Term>,
    a: PyTerm,
    b: PyTerm,
) -> Result<PyTerm, String> {
    match (&a.ty, &b.ty, fu, ff, fb) {
        (Ty::Uint(na), Ty::Uint(nb), Some(fu), _, _) if na == nb => {
            Ok(PyTerm::new(Ty::Bool, fu(a.term.clone(), b.term.clone())))
        }
        (Ty::Bool, Ty::Bool, _, _, Some(fb)) => {
            Ok(PyTerm::new(Ty::Bool, fb(a.term.clone(), b.term.clone())))
        }
        (Ty::Field, Ty::Field, _, Some(ff), _) => {
            Ok(PyTerm::new(Ty::Bool, ff(a.term.clone(), b.term.clone())))
        }
        (x, y, _, _, _) => Err(format!("Cannot perform op '{name}' on {x} and {y}")),
    }
}

fn add_uint(a: Term, b: Term) -> Term {
    term![Op::BvNaryOp(BvNaryOp::Add); a, b]
}

fn add_field(a: Term, b: Term) -> Term {
    term![Op::PfNaryOp(PfNaryOp::Add); a, b]
}

pub fn add(a: PyTerm, b: PyTerm) -> Result<PyTerm, String> {
    wrap_bin_op("+", Some(add_uint), Some(add_field), None, a, b)
}

fn sub_uint(a: Term, b: Term) -> Term {
    term![Op::BvBinOp(BvBinOp::Sub); a, b]
}

fn sub_field(a: Term, b: Term) -> Term {
    term![Op::PfNaryOp(PfNaryOp::Add); a, term![Op::PfUnOp(PfUnOp::Neg); b]]
}

pub fn sub(a: PyTerm, b: PyTerm) -> Result<PyTerm, String> {
    wrap_bin_op("-", Some(sub_uint), Some(sub_field), None, a, b)
}

fn mul_uint(a: Term, b: Term) -> Term {
    term![Op::BvNaryOp(BvNaryOp::Mul); a, b]
}

fn mul_field(a: Term, b: Term) -> Term {
    term![Op::PfNaryOp(PfNaryOp::Mul); a, b]
}

pub fn mul(a: PyTerm, b: PyTerm) -> Result<PyTerm, String> {
    wrap_bin_op("*", Some(mul_uint), Some(mul_field), None, a, b)
}

fn div_uint(a: Term, b: Term) -> Term {
    term![Op::BvBinOp(BvBinOp::Udiv); a, b]
}

fn div_field(a: Term, b: Term) -> Term {
    term![Op::PfNaryOp(PfNaryOp::Mul); a, term![Op::PfUnOp(PfUnOp::Recip); b]]
}

pub fn div(a: PyTerm, b: PyTerm) -> Result<PyTerm, String> {
    wrap_bin_op("/", Some(div_uint), Some(div_field), None, a, b)
}

fn rem_field(a: Term, b: Term) -> Term {
    let len = cfg().field().modulus().significant_bits() as usize;
    let a_bv = term![Op::PfToBv(len); a];
    let b_bv = term![Op::PfToBv(len); b];
    term![Op::UbvToPf(default_field()); term![Op::BvBinOp(BvBinOp::Urem); a_bv, b_bv]]
}

fn rem_uint(a: Term, b: Term) -> Term {
    term![Op::BvBinOp(BvBinOp::Urem); a, b]
}

pub fn rem(a: PyTerm, b: PyTerm) -> Result<PyTerm, String> {
    wrap_bin_op("%", Some(rem_uint), Some(rem_field), None, a, b)
}

fn bitand_uint(a: Term, b: Term) -> Term {
    term![Op::BvNaryOp(BvNaryOp::And); a, b]
}

pub fn bitand(a: PyTerm, b: PyTerm) -> Result<PyTerm, String> {
    wrap_bin_op("&", Some(bitand_uint), None, None, a, b)
}

fn bitor_uint(a: Term, b: Term) -> Term {
    term![Op::BvNaryOp(BvNaryOp::Or); a, b]
}

pub fn bitor(a: PyTerm, b: PyTerm) -> Result<PyTerm, String> {
    wrap_bin_op("|", Some(bitor_uint), None, None, a, b)
}

fn bitxor_uint(a: Term, b: Term) -> Term {
    term![Op::BvNaryOp(BvNaryOp::Xor); a, b]
}

pub fn bitxor(a: PyTerm, b: PyTerm) -> Result<PyTerm, String> {
    wrap_bin_op("^", Some(bitxor_uint), None, None, a, b)
}

fn or_bool(a: Term, b: Term) -> Term {
    term![Op::BoolNaryOp(BoolNaryOp::Or); a, b]
}

pub fn or(a: PyTerm, b: PyTerm) -> Result<PyTerm, String> {
    wrap_bin_op("or", None, None, Some(or_bool), a, b)
}

fn and_bool(a: Term, b: Term) -> Term {
    term![Op::BoolNaryOp(BoolNaryOp::And); a, b]
}

pub fn and(a: PyTerm, b: PyTerm) -> Result<PyTerm, String> {
    wrap_bin_op("and", None, None, Some(and_bool), a, b)
}

fn eq_base(a: PyTerm, b: PyTerm) -> Result<Term, String> {
    if a.ty != b.ty {
        Err(format!(
            "Cannot '==' dissimilar types {} and {}",
            a.type_(),
            b.type_()
        ))
    } else {
        Ok(term![Op::Eq; a.term, b.term])
    }
}

pub fn eq(a: PyTerm, b: PyTerm) -> Result<PyTerm, String> {
    Ok(PyTerm::new(Ty::Bool, eq_base(a, b)?))
}

pub fn neq(a: PyTerm, b: PyTerm) -> Result<PyTerm, String> {
    Ok(PyTerm::new(Ty::Bool, not_bool(eq_base(a, b)?)))
}

fn ult_uint(a: Term, b: Term) -> Term {
    term![Op::BvBinPred(BvBinPred::Ult); a, b]
}

fn field_comp(a: Term, b: Term, op: BvBinPred) -> Term {
    let len = cfg().field().modulus().significant_bits() as usize;
    let a_bv = term![Op::PfToBv(len); a];
    let b_bv = term![Op::PfToBv(len); b];
    term![Op::BvBinPred(op); a_bv, b_bv]
}

fn ult_field(a: Term, b: Term) -> Term {
    field_comp(a, b, BvBinPred::Ult)
}

pub fn ult(a: PyTerm, b: PyTerm) -> Result<PyTerm, String> {
    wrap_bin_pred("<", Some(ult_uint), Some(ult_field), None, a, b)
}

fn ule_uint(a: Term, b: Term) -> Term {
    term![Op::BvBinPred(BvBinPred::Ule); a, b]
}

fn ule_field(a: Term, b: Term) -> Term {
    field_comp(a, b, BvBinPred::Ule)
}

pub fn ule(a: PyTerm, b: PyTerm) -> Result<PyTerm, String> {
    wrap_bin_pred("<=", Some(ule_uint), Some(ule_field), None, a, b)
}

fn ugt_uint(a: Term, b: Term) -> Term {
    term![Op::BvBinPred(BvBinPred::Ugt); a, b]
}

fn ugt_field(a: Term, b: Term) -> Term {
    field_comp(a, b, BvBinPred::Ugt)
}

pub fn ugt(a: PyTerm, b: PyTerm) -> Result<PyTerm, String> {
    wrap_bin_pred(">", Some(ugt_uint), Some(ugt_field), None, a, b)
}

fn uge_uint(a: Term, b: Term) -> Term {
    term![Op::BvBinPred(BvBinPred::Uge); a, b]
}

fn uge_field(a: Term, b: Term) -> Term {
    field_comp(a, b, BvBinPred::Uge)
}

pub fn uge(a: PyTerm, b: PyTerm) -> Result<PyTerm, String> {
    wrap_bin_pred(">=", Some(uge_uint), Some(uge_field), None, a, b)
}

pub fn pow(a: PyTerm, b: PyTerm) -> Result<PyTerm, String> {
    if a.ty != Ty::Field || b.ty != Ty::Uint(32) {
        return Err(format!("Cannot compute {a} ** {b} : must be Field ** U32"));
    }

    let a = a.term;
    let b = const_int(b)?;
    if b == 0 {
        return Ok(field_lit(1));
    }

    let res = (0..b.significant_bits() - 1)
        .rev()
        .fold(a.clone(), |acc, ix| {
            let acc = mul_field(acc.clone(), acc);
            if b.get_bit(ix) {
                mul_field(acc, a.clone())
            } else {
                acc
            }
        });
    Ok(PyTerm::new(Ty::Field, res))
}

fn wrap_un_op(
    name: &str,
    fu: Option<fn(Term) -> Term>,
    ff: Option<fn(Term) -> Term>,
    fb: Option<fn(Term) -> Term>,
    a: PyTerm,
) -> Result<PyTerm, String> {
    match (&a.ty, fu, ff, fb) {
        (Ty::Uint(_), Some(fu), _, _) => Ok(PyTerm::new(a.ty.clone(), fu(a.term.clone()))),
        (Ty::Bool, _, _, Some(fb)) => Ok(PyTerm::new(Ty::Bool, fb(a.term.clone()))),
        (Ty::Field, _, Some(ff), _) => Ok(PyTerm::new(Ty::Field, ff(a.term.clone()))),
        (x, _, _, _) => Err(format!("Cannot perform op '{name}' on {x}")),
    }
}

fn neg_field(a: Term) -> Term {
    term![Op::PfUnOp(PfUnOp::Neg); a]
}

fn neg_uint(a: Term) -> Term {
    term![Op::BvUnOp(BvUnOp::Neg); a]
}

// note: if circ doesn't support bitwise not then we won't have it in python
pub fn neg(a: PyTerm) -> Result<PyTerm, String> {
    wrap_un_op("unary-", Some(neg_uint), Some(neg_field), None, a)
}

fn not_bool(a: Term) -> Term {
    term![Op::Not; a]
}

fn not_uint(a: Term) -> Term {
    term![Op::BvUnOp(BvUnOp::Not); a]
}

pub fn not(a: PyTerm) -> Result<PyTerm, String> {
    wrap_un_op("not", Some(not_uint), None, Some(not_bool), a)
}

pub fn const_int(a: PyTerm) -> Result<Integer, String> {
    match const_value(&a.term) {
        Some(Value::Field(f)) => Ok(f.i()),
        Some(Value::BitVector(f)) => Ok(f.uint().clone()),
        _ => Err(format!("{a} is not a constant integer")),
    }
}

pub fn const_bool(a: PyTerm) -> Option<bool> {
    match const_value(&a.term) {
        Some(Value::Bool(b)) => Some(b),
        _ => None,
    }
}

pub fn const_val(a: PyTerm) -> Result<PyTerm, String> {
    match const_value(&a.term) {
        Some(v) => Ok(PyTerm::new(a.ty, leaf_term(Op::Const(v)))),
        _ => Err(format!("{} is not a constant value", &a)),
    }
}

fn const_value(t: &Term) -> Option<Value> {
    let folded = constant_fold(t, &[]);
    match &folded.op() {
        Op::Const(v) => Some(v.clone()),
        _ => None,
    }
}

pub fn bool(a: PyTerm) -> Result<Term, String> {
    match &a.ty {
        Ty::Bool => Ok(a.term),
        a => Err(format!("{a} is not a boolean")),
    }
}

fn wrap_shift(name: &str, op: BvBinOp, a: PyTerm, b: PyTerm) -> Result<PyTerm, String> {
    let bc = const_int(b)?;
    match &a.ty {
        &Ty::Uint(na) => Ok(PyTerm::new(a.ty, term![Op::BvBinOp(op); a.term, bv_lit(bc, na)])),
        x => Err(format!("Cannot perform op '{name}' on {x} and {bc}")),
    }
}

pub fn shl(a: PyTerm, b: PyTerm) -> Result<PyTerm, String> {
    wrap_shift("<<", BvBinOp::Shl, a, b)
}

pub fn shr(a: PyTerm, b: PyTerm) -> Result<PyTerm, String> {
    wrap_shift(">>", BvBinOp::Lshr, a, b)
}

fn ite(c: Term, a: PyTerm, b: PyTerm) -> Result<PyTerm, String> {
    if a.ty != b.ty {
        Err(format!("Cannot perform ITE on {a} and {b}"))
    } else {
        Ok(PyTerm::new(a.ty.clone(), term![Op::Ite; c, a.term, b.term]))
    }
}

pub fn cond(c: PyTerm, a: PyTerm, b: PyTerm) -> Result<PyTerm, String> {
    ite(bool(c)?, a, b)
}

pub fn pf_lit_ir<I>(i: I) -> Term
where
    Integer: From<I>,
{
    leaf_term(Op::Const(pf_val(i)))
}

fn pf_val<I>(i: I) -> Value
where
    Integer: From<I>,
{
    Value::Field(cfg().field().new_v(i))
}

pub fn field_lit<I>(i: I) -> PyTerm
where
    Integer: From<I>,
{
    PyTerm::new(Ty::Field, pf_lit_ir(i))
}

pub fn py_bool_lit(v: bool) -> PyTerm {
    PyTerm::new(Ty::Bool, leaf_term(Op::Const(Value::Bool(v))))
}

pub fn uint_lit<I>(v: I, bits: usize) -> PyTerm
where
    Integer: From<I>,
{
    PyTerm::new(Ty::Uint(bits), bv_lit(v, bits))
}

pub fn slice(arr: PyTerm, start: Option<usize>, end: Option<usize>) -> Result<PyTerm, String> {
    match &arr.ty {
        Ty::Array(size, _) => {
            let start = start.unwrap_or(0);
            let end = end.unwrap_or(*size);
            array(arr.unwrap_array()?.drain(start..end))
        }
        Ty::MutArray(size) => {
            let start = start.unwrap_or(0);
            let end = end.unwrap_or(*size);
            array(arr.unwrap_array()?.drain(start..end))
        }
        a => Err(format!("Cannot slice {a}")),
    }
}

pub fn field_select(class_: &PyTerm, field: &str) -> Result<PyTerm, String> {
    match &class_.ty {
        Ty::DataClass(_, map) => {
            if let Some((idx, ty)) = map.search(field) {
                Ok(PyTerm::new(
                    ty.clone(),
                    term![Op::Field(idx); class_.term.clone()],
                ))
            } else {
                Err(format!("No field '{field}'"))
            }
        }
        a => Err(format!("{a} is not a class")),
    }
}

pub fn field_store(class_: PyTerm, field: &str, val: PyTerm) -> Result<PyTerm, String> {
    match &class_.ty {
        Ty::DataClass(_, map) => {
            if let Some((idx, ty)) = map.search(field) {
                if ty == &val.ty {
                    Ok(PyTerm::new(
                        class_.ty.clone(),
                        term![Op::Update(idx); class_.term.clone(), val.term],
                    ))
                } else {
                    Err(format!(
                        "term {val} assigned to field {field} of type {}",
                        map.get(idx).1
                    ))
                }
            } else {
                Err(format!("No field '{field}'"))
            }
        }
        a => Err(format!("{a} is not a class")),
    }
}

fn coerce_to_field(i: PyTerm) -> Result<Term, String> {
    match &i.ty {
        Ty::Uint(_) => Ok(term![Op::UbvToPf(default_field()); i.term]),
        Ty::Field => Ok(i.term),
        _ => Err(format!("Cannot coerce {} to a field element", &i)),
    }
}

pub fn array_select(array: PyTerm, idx: PyTerm) -> Result<PyTerm, String> {
    match array.ty {
        Ty::Array(_, elem_ty) if matches!(idx.ty, Ty::Uint(_) | Ty::Field) => {
            let iterm = coerce_to_field(idx).unwrap();
            Ok(PyTerm::new(*elem_ty, term![Op::Select; array.term, iterm]))
        }
        Ty::MutArray(_) if matches!(idx.ty, Ty::Uint(_) | Ty::Field) => {
            let iterm = coerce_to_field(idx).unwrap();
            Ok(PyTerm::new(Ty::Field, term![Op::Select; array.term, iterm]))
        }
        _ => Err(format!("Cannot index {} using {}", &array.ty, &idx.ty)),
    }
}

pub fn mut_array_store(array: PyTerm, idx: PyTerm, val: PyTerm, cond: Term) -> Result<PyTerm, String> {
    if !matches!(array.ty, Ty::MutArray(_) | Ty::Array(..)) {
        return Err(format!(
            "Can only call mut_array_store on arrays, not {array}"
        ));
    }
    let i = coerce_to_field(idx).map_err(|s| format!("{s}: mutable array index"))?;
    let v = coerce_to_field(val).map_err(|s| format!("{s}: mutable array value"))?;
    Ok(PyTerm::new(array.ty, term![Op::CStore; array.term, i, v, cond]))
}

pub fn array_store(array: PyTerm, idx: PyTerm, val: PyTerm) -> Result<PyTerm, String> {
    if matches!(&array.ty, Ty::Array(_, _)) && matches!(&idx.ty, Ty::Uint(_) | Ty::Field) {
        let iterm = if matches!(idx.ty, Ty::Uint(_)) {
            term![Op::UbvToPf(default_field()); idx.term]
        } else {
            idx.term
        };
        Ok(PyTerm::new(
            array.ty,
            term![Op::Store; array.term, iterm, val.term],
        ))
    } else {
        Err(format!("Cannot index {} using {}", &array.ty, &idx.ty))
    }
}

fn ir_array<I: IntoIterator<Item = Term>>(value_sort: Sort, elems: I) -> Term {
    let key_sort = Sort::Field(cfg().field().clone());
    term(Op::Array(key_sort, value_sort), elems.into_iter().collect())
}

pub fn fill_array(value: PyTerm, size: usize) -> Result<PyTerm, String> {
    Ok(PyTerm::new(
        Ty::Array(size, Box::new(value.ty)),
        term![Op::Fill(default_field_sort(), size); value.term],
    ))
}

pub fn array<I: IntoIterator<Item = PyTerm>>(elems: I) -> Result<PyTerm, String> {
    let v: Vec<PyTerm> = elems.into_iter().collect();
    if let Some(e) = v.first() {
        let ty = e.type_();
        if v.iter().skip(1).any(|a| a.type_() != ty) {
            Err("Inconsistent types in array".to_string())
        } else {
            let sort = check(&e.term);
            Ok(PyTerm::new(
                Ty::Array(v.len(), Box::new(ty.clone())),
                ir_array(sort, v.into_iter().map(|t| t.term)),
            ))
        }
    } else {
        Err("Empty array".to_string())
    }
}

pub fn uint_to_field(u: PyTerm) -> Result<PyTerm, String> {
    match &u.ty {
        Ty::Uint(_) => Ok(PyTerm::new(
            Ty::Field,
            term![Op::UbvToPf(default_field()); u.term],
        )),
        u => Err(format!("Cannot do uint-to-field on {u}")),
    }
}

// pub fn uint_to_uint(u: PyTerm, w: usize) -> Result<PyTerm, String> {
//     match &u.ty {
//         Ty::Uint(n) if *n <= w => Ok(PyTerm::new(Ty::Uint(w), term![Op::BvUext(w - n); u.term])),
//         Ty::Uint(n) => Err(format!("Tried narrowing uint{n}-to-uint{w} attempted")),
//         u => Err(format!("Cannot do uint-to-uint on {u}")),
//     }
// }

pub fn uint_to_bits(u: PyTerm) -> Result<PyTerm, String> {
    match &u.ty {
        Ty::Uint(n) => Ok(PyTerm::new(
            Ty::Array(*n, Box::new(Ty::Bool)),
            ir_array(
                Sort::Bool,
                (0..*n).rev().map(|i| term![Op::BvBit(i); u.term.clone()]),
            ),
        )),
        u => Err(format!("Cannot do uint-to-bits on {u}")),
    }
}

pub fn uint_from_bits(u: PyTerm) -> Result<PyTerm, String> {
    match &u.ty {
        Ty::Array(bits, elem_ty) if **elem_ty == Ty::Bool => match bits {
            8 | 16 | 32 | 64 => Ok(PyTerm::new(
                Ty::Uint(*bits),
                term(
                    Op::BvConcat,
                    u.unwrap_array_ir()?
                        .into_iter()
                        .map(|z: Term| -> Term { term![Op::BoolToBv; z] })
                        .collect(),
                ),
            )),
            l => Err(format!("Cannot do uint-from-bits on len {l} array")),
        },
        u => Err(format!("Cannot do uint-from-bits on {u}")),
    }
}

pub fn field_to_bits(f: PyTerm, n: usize) -> Result<PyTerm, String> {
    match &f.ty {
        Ty::Field => uint_to_bits(PyTerm::new(Ty::Uint(n), term![Op::PfToBv(n); f.term])),
        u => Err(format!("Cannot do uint-to-bits on {u}")),
    }
}

fn bv_from_bits(barr: Term, size: usize) -> Term {
    term(
        Op::BvConcat,
        (0..size)
            .map(|i| term![Op::BoolToBv; term![Op::Select; barr.clone(), pf_lit_ir(i)]])
            .collect(),
    )
}

pub fn bit_array_le(a: PyTerm, b: PyTerm, n: usize) -> Result<PyTerm, String> {
    match (&a.ty, &b.ty) {
        (Ty::Array(la, ta), Ty::Array(lb, tb)) => {
            if **ta != Ty::Bool || **tb != Ty::Bool {
                Err("bit-array-le must be called on arrays of Bools".to_string())
            } else if la != lb {
                Err(format!(
                    "bit-array-le called on arrays with lengths {la} != {lb}"
                ))
            } else if *la != n {
                Err(format!(
                    "bit-array-le::<{n}> called on arrays with length {la}"
                ))
            } else {
                Ok(())
            }
        }
        _ => Err(format!("Cannot do bit-array-le on ({}, {})", &a.ty, &b.ty)),
    }?;

    let at = bv_from_bits(a.term, n);
    let bt = bv_from_bits(b.term, n);
    Ok(PyTerm::new(
        Ty::Bool,
        term![Op::BvBinPred(BvBinPred::Ule); at, bt],
    ))
}

pub struct Python {}

fn field_name(class_name: &str, field_name: &str) -> String {
    format!("{class_name}.{field_name}")
}

fn idx_name(class_name: &str, idx: usize) -> String {
    format!("{class_name}.{idx}")
}

impl Python {
    pub fn new() -> Self {
        Self {}
    }
}

impl Typed<Ty> for PyTerm {
    fn type_(&self) -> Ty {
        self.ty.clone()
    }
}

impl Embeddable for Python {
    type T = PyTerm;
    type Ty = Ty;

    fn declare_input(
        &self,
        ctx: &mut CirCtx,
        ty: &Self::Ty,
        name: String,
        visibility: Option<PartyId>,
        precompute: Option<Self::T>
    ) -> Self::T {
        match ty {
            Ty::Bool => Self::T::new(
                Ty::Bool,
                ctx.cs.borrow_mut().new_var(
                    &name,
                    Sort::Bool,
                    visibility,
                    precompute.map(|p| p.term),
                ),
            ),
            Ty::Field => Self::T::new(
                Ty::Field,
                ctx.cs.borrow_mut().new_var(
                    &name,
                    default_field_sort(),
                    visibility,
                    precompute.map(|p| p.term),
                ),
            ),
            Ty::Uint(w) => Self::T::new(
                Ty::Uint(*w),
                ctx.cs.borrow_mut().new_var(
                    &name,
                    Sort::BitVector(*w),
                    visibility,
                    precompute.map(|p| p.term),
                ),
            ),
            Ty::Array(n, ty) => {
                let ps: Vec<Option<Self::T>> = match precompute.map(|p| p.unwrap_array()) {
                    Some(Ok(v)) => v.into_iter().map(Some).collect(),
                    Some(Err(e)) => panic!("{}", e),
                    None => std::iter::repeat(None).take(*n).collect(),
                };
                debug_assert_eq!(*n, ps.len());
                array(
                    ps.into_iter().enumerate().map(|(i, p)| {
                        self.declare_input(ctx, ty, idx_name(&name, i), visibility, p)
                    }),
                )
                .unwrap()
            },
            Ty::MutArray(n) => {
                let ps: Vec<Option<Self::T>> = match precompute.map(|p| p.unwrap_array()) {
                    Some(Ok(v)) => v.into_iter().map(Some).collect(),
                    Some(Err(e)) => panic!("{}", e),
                    None => std::iter::repeat(None).take(*n).collect(),
                };
                debug_assert_eq!(*n, ps.len());
                array(
                    ps.into_iter().enumerate().map(|(i, p)| {
                        self.declare_input(ctx, &Ty::Field, idx_name(&name, i), visibility, p)
                    }),
                )
                .unwrap()
            },
            Ty::DataClass(n, fs) => Self::T::new_class(
                n.clone(),
                fs.fields()
                    .map(|(f_name, f_ty)| {
                        (
                            f_name.clone(),
                            self.declare_input(
                                ctx,
                                f_ty,
                                field_name(&name, f_name),
                                visibility,
                                precompute.as_ref().map(|_| unimplemented!("precomputations for declared inputs that are Python classes")),
                            ),
                        )
                    })
                    .collect(),
            ),
        }
    }

    fn ite(&self, _ctx: &mut CirCtx, cond: Term, t: Self::T, f: Self::T) -> Self::T {
        ite(cond, t, f).unwrap()
    }

    fn create_uninit(&self, _ctx: &mut CirCtx, ty: &Self::Ty) -> Self::T {
        ty.default()
    }

    fn initialize_return(&self, ty: &Self::Ty, _ssa_name: &String) -> Self::T {
        ty.default()
    }

    fn wrap_persistent_array(&self, t: Term) -> Self::T {
        let size = check(&t).as_array().2;
        Self::T::new(Ty::MutArray(size), t)
    }
}
