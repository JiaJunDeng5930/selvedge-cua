From Stdlib Require Import Bool NArith.
Open Scope N_scope.

(* A reference points to a retained platform element and process identity.
   A bounded read window limits those native handles. An app exiting
   invalidates its references independently of their age. *)
Definition retained_read (age : N) : bool := age <? 8.

Definition reference_live (age : N) (terminated : bool) : bool :=
  retained_read age && negb terminated.

Theorem reference_live_contract age terminated :
  reference_live age terminated = true <-> age < 8 /\ terminated = false.
Proof.
  unfold reference_live, retained_read.
  rewrite andb_true_iff, N.ltb_lt, negb_true_iff. reflexivity.
Qed.

(* Text dispatch requires the platform's observations of an editable field
   and matching keyboard focus within the target process. These observations
   authorize an attempt at dispatch. They do not freeze the desktop. *)
Inductive insertion := Insert | NotEditable | NotFocused.

Definition authorize_insertion (text_role writable disabled same_element : bool)
  : insertion :=
  if text_role && writable && negb disabled then
    if same_element then Insert else NotFocused
  else NotEditable.

Theorem authorize_insertion_contract role writable disabled focused :
  authorize_insertion role writable disabled focused = Insert <->
  role = true /\ writable = true /\ disabled = false /\ focused = true.
Proof. destruct role, writable, disabled, focused; simpl; intuition discriminate. Qed.
