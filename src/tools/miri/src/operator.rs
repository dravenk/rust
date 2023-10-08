use std::iter;

use log::trace;

use rand::{seq::IteratorRandom, Rng};
use rustc_apfloat::Float;
use rustc_middle::mir;
use rustc_target::abi::Size;

use crate::*;

impl<'mir, 'tcx: 'mir> EvalContextExt<'mir, 'tcx> for crate::MiriInterpCx<'mir, 'tcx> {}
pub trait EvalContextExt<'mir, 'tcx: 'mir>: crate::MiriInterpCxExt<'mir, 'tcx> {
    fn binary_ptr_op(
        &self,
        bin_op: mir::BinOp,
        left: &ImmTy<'tcx, Provenance>,
        right: &ImmTy<'tcx, Provenance>,
    ) -> InterpResult<'tcx, (ImmTy<'tcx, Provenance>, bool)> {
        use rustc_middle::mir::BinOp::*;

        let this = self.eval_context_ref();
        trace!("ptr_op: {:?} {:?} {:?}", *left, bin_op, *right);

        Ok(match bin_op {
            Eq | Ne | Lt | Le | Gt | Ge => {
                assert_eq!(left.layout.abi, right.layout.abi); // types an differ, e.g. fn ptrs with different `for`
                let size = this.pointer_size();
                // Just compare the bits. ScalarPairs are compared lexicographically.
                // We thus always compare pairs and simply fill scalars up with 0.
                let left = match **left {
                    Immediate::Scalar(l) => (l.to_bits(size)?, 0),
                    Immediate::ScalarPair(l1, l2) => (l1.to_bits(size)?, l2.to_bits(size)?),
                    Immediate::Uninit => panic!("we should never see uninit data here"),
                };
                let right = match **right {
                    Immediate::Scalar(r) => (r.to_bits(size)?, 0),
                    Immediate::ScalarPair(r1, r2) => (r1.to_bits(size)?, r2.to_bits(size)?),
                    Immediate::Uninit => panic!("we should never see uninit data here"),
                };
                let res = match bin_op {
                    Eq => left == right,
                    Ne => left != right,
                    Lt => left < right,
                    Le => left <= right,
                    Gt => left > right,
                    Ge => left >= right,
                    _ => bug!(),
                };
                (ImmTy::from_bool(res, *this.tcx), false)
            }

            // Some more operations are possible with atomics.
            // The return value always has the provenance of the *left* operand.
            Add | Sub | BitOr | BitAnd | BitXor => {
                assert!(left.layout.ty.is_unsafe_ptr());
                assert!(right.layout.ty.is_unsafe_ptr());
                let ptr = left.to_scalar().to_pointer(this)?;
                // We do the actual operation with usize-typed scalars.
                let left = ImmTy::from_uint(ptr.addr().bytes(), this.machine.layouts.usize);
                let right = ImmTy::from_uint(
                    right.to_scalar().to_target_usize(this)?,
                    this.machine.layouts.usize,
                );
                let (result, overflowing) = this.overflowing_binary_op(bin_op, &left, &right)?;
                // Construct a new pointer with the provenance of `ptr` (the LHS).
                let result_ptr = Pointer::new(
                    ptr.provenance,
                    Size::from_bytes(result.to_scalar().to_target_usize(this)?),
                );
                (
                    ImmTy::from_scalar(Scalar::from_maybe_pointer(result_ptr, this), left.layout),
                    overflowing,
                )
            }

            _ => span_bug!(this.cur_span(), "Invalid operator on pointers: {:?}", bin_op),
        })
    }

    fn generate_nan<F: Float>(&self, inputs: &[F]) -> F {
        let this = self.eval_context_ref();
        let mut rand = this.machine.rng.borrow_mut();
        // Assemble an iterator of possible NaNs: preferred, unchanged propagation, quieting propagation.
        let preferred_nan = F::qnan(Some(0));
        let nans = iter::once(preferred_nan)
            .chain(inputs.iter().filter(|f| f.is_nan()).copied())
            .chain(inputs.iter().filter(|f| f.is_signaling()).map(|f| {
                // Make it quiet, by setting the bit. We assume that `preferred_nan`
                // only has bits set that all quiet NaNs need to have set.
                F::from_bits(f.to_bits() | preferred_nan.to_bits())
            }));
        // Pick one of the NaNs.
        let nan = nans.choose(&mut *rand).unwrap();
        // Non-deterministically flip the sign.
        if rand.gen() {
            // This will properly flip even for NaN.
            -nan
        } else {
            nan
        }
    }
}
