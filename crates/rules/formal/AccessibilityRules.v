From Stdlib Require Import Bool NArith Lia.
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

(* Matching geometry identifies a single destination when possible. A title
   only disambiguates matching geometry when exactly one candidate has it. *)
Inductive window_match := Unmatched | Ambiguous | Geometry | Title.

Definition choose_window (geometry title : N) : window_match :=
  if geometry =? 0 then Unmatched else
  if geometry =? 1 then Geometry else
  if title =? 1 then Title else Ambiguous.

Theorem choose_window_geometry_contract geometry title :
  choose_window geometry title = Geometry <-> geometry = 1.
Proof.
  unfold choose_window. destruct (geometry =? 0) eqn:empty.
  - apply N.eqb_eq in empty. subst. split; discriminate.
  - destruct (geometry =? 1) eqn:one.
    + now rewrite N.eqb_eq in one.
    + rewrite N.eqb_neq in one. destruct (title =? 1); intuition discriminate.
Qed.

Theorem choose_window_title_contract geometry title :
  choose_window geometry title = Title <-> 1 < geometry /\ title = 1.
Proof.
  unfold choose_window. destruct (geometry =? 0) eqn:empty.
  - apply N.eqb_eq in empty. subst. split; [discriminate | intros [? _]; lia].
  - destruct (geometry =? 1) eqn:one.
    + apply N.eqb_eq in one. subst. split; [discriminate | intros [? _]; lia].
    + apply N.eqb_neq in empty, one. destruct (title =? 1) eqn:named.
      * apply N.eqb_eq in named. split; [intros _; split; lia | reflexivity].
      * apply N.eqb_neq in named. intuition discriminate.
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
