From Stdlib Require Import NArith Arith List Lia.
Import ListNotations.
Open Scope N_scope.

(* Position functions live in the script engine. The shared sampling rule
   supplies time offsets so there is a sample at the start and at the exact
   end, including when the duration is shorter than one interval. *)
Fixpoint interior (count : nat) (interval : N) (tail : list N) : list N :=
  match count with
  | O => tail
  | S previous =>
      interior previous interval (N.of_nat count * interval :: tail)
  end.

Definition sample_times (duration interval : N) : option (list N) :=
  if N.eqb interval 0 then None
  else if N.eqb duration 0 then Some [0]
  else
    let count := (duration - 1) / interval in
    if count <? 1023
    then Some (0 :: interior (N.to_nat count) interval [duration])
    else None.

Lemma interior_nonempty count interval t rest :
  exists head others, interior count interval (t :: rest) = head :: others.
Proof.
  revert t rest.
  induction count as [| count IH]; intros t rest; simpl.
  - eauto.
  - apply IH.
Qed.

Lemma interior_last count interval t rest fallback :
  last (interior count interval (t :: rest)) fallback = last (t :: rest) fallback.
Proof.
  revert t rest.
  induction count as [| count IH]; intros t rest; simpl; [reflexivity |].
  rewrite IH. reflexivity.
Qed.

Lemma interior_length count interval tail :
  length (interior count interval tail) = (count + length tail)%nat.
Proof.
  revert tail.
  induction count as [| count IH]; intro tail; simpl; [reflexivity |].
  rewrite IH. simpl. lia.
Qed.

Theorem sample_times_contract duration interval samples :
  sample_times duration interval = Some samples ->
  hd 1 samples = 0 /\ last samples 1 = duration /\ (length samples <= 1024)%nat.
Proof.
  unfold sample_times.
  destruct (interval =? 0); [discriminate |].
  destruct (duration =? 0) eqn:Hzero.
  - intros H. inversion H; subst. apply N.eqb_eq in Hzero. subst. simpl. lia.
  - set (count := (duration - 1) / interval).
    destruct (count <? 1023) eqn:Hcount; [|discriminate].
    intros H. inversion H; subst. simpl.
    destruct (interior_nonempty (N.to_nat count) interval duration []) as [head [tail Hint]].
    rewrite Hint. split; [reflexivity |].
    split.
    + rewrite <- Hint. apply interior_last.
    + change (S (length (head :: tail)) <= 1024)%nat.
      rewrite <- Hint, interior_length.
      apply N.ltb_lt in Hcount.
      apply N.compare_lt_iff in Hcount.
      rewrite N2Nat.inj_compare in Hcount.
      apply Nat.compare_lt_iff in Hcount.
      change (N.to_nat count < 1023)%nat in Hcount.
      change (S (N.to_nat count + 1) <= 1024)%nat.
      lia.
Qed.

Theorem sample_times_single interval :
  0 < interval -> sample_times 0 interval = Some [0].
Proof.
  intros H. unfold sample_times.
  assert ((interval =? 0) = false) as -> by (apply N.eqb_neq; lia).
  reflexivity.
Qed.
