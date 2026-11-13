From Stdlib Require Import List NArith Arith Lia.
Import ListNotations.
Open Scope N_scope.

(* The capture adapter owns pixels and measures equality, a changed
   rectangle and any supported translation. The planner decides what the
   model sees. Frame numbers refer to the original ordered observation. *)
Definition frame_span := (N * N)%type.

Fixpoint group_after (same_as_previous : list bool) (first current : N)
  : list frame_span :=
  match same_as_previous with
  | [] => [(first, current)]
  | true :: rest => group_after rest first (N.succ current)
  | false :: rest => (first, current) :: group_after rest (N.succ current) (N.succ current)
  end.

Definition group_frames (same_as_previous : list bool) : list frame_span :=
  group_after same_as_previous 0 0.

Lemma group_after_nonempty flags first current :
  group_after flags first current <> [].
Proof.
  revert first current.
  induction flags as [| [] rest IH]; intros first current; simpl;
    try discriminate; apply IH.
Qed.

Lemma group_after_last flags first current :
  snd (last (group_after flags first current) (0, 0)) =
  current + N.of_nat (length flags).
Proof.
  revert first current.
  induction flags as [| equal rest IH]; intros first current.
  - simpl. now rewrite N.add_0_r.
  - rewrite List.length_cons, Nat2N.inj_succ, <- N.add_succ_comm.
    destruct equal.
    + apply IH.
    + simpl group_after.
      specialize (IH (N.succ current) (N.succ current)).
      pose proof (group_after_nonempty rest (N.succ current) (N.succ current)) as Hnonempty.
      destruct (group_after rest (N.succ current) (N.succ current)); [contradiction | exact IH].
Qed.

Theorem group_frames_preserves_latest flags :
  snd (last (group_frames flags) (0, 0)) = N.of_nat (length flags).
Proof.
  unfold group_frames. apply group_after_last.
Qed.

(* Area is measured in pixels. A spatially small change needs the before
   and after crop. Changes spread across the viewport use a keyframe. *)
Definition local_change (changed_area viewport_area : N) : bool :=
  (3 * changed_area <? viewport_area).

Theorem local_change_contract changed viewport :
  local_change changed viewport = true <-> 3 * changed < viewport.
Proof. apply N.ltb_lt. Qed.

(* A translation joins the preceding mosaic exactly when its geometry and
   direction match. Pixel offsets can change with input speed. *)
Fixpoint same_scroll (first next : list N) : bool :=
  match first, next with
  | [], [] => true
  | a :: rest, b :: tail => (a =? b) && same_scroll rest tail
  | _, _ => false
  end.

Theorem same_scroll_contract first next :
  same_scroll first next = true <-> first = next.
Proof.
  revert next.
  induction first as [| first rest IH]; intros [| next tail]; simpl;
    try (split; congruence).
  rewrite Bool.andb_true_iff, N.eqb_eq, IH.
  split.
  - intros [-> ->]. reflexivity.
  - intro H. inversion H. auto.
Qed.

Inductive image_kind := Keyframe | Change | Scroll | Viewport.

Definition priority (kind : image_kind) : N :=
  match kind with Keyframe => 0 | Change => 1 | Scroll => 2 | Viewport => 3 end.

Definition pick (best candidate : N * image_kind) : N * image_kind :=
  if priority (snd best) <=? priority (snd candidate) then candidate else best.

(* Candidates in each chronological bucket compete for that bucket's slot. *)
Definition select_bucket (bucket : list (N * image_kind)) : option N :=
  match bucket with
  | [] => None
  | first :: rest => Some (fst (fold_left pick rest first))
  end.

Lemma pick_from_either a b : pick a b = a \/ pick a b = b.
Proof. unfold pick. destruct (_ <=? _); auto. Qed.

Lemma fold_pick_member rest first :
  In (fold_left pick rest first) (first :: rest).
Proof.
  revert first.
  induction rest as [| next rest IH]; intro first; simpl; [auto |].
  specialize (IH (pick first next)).
  destruct (pick_from_either first next) as [H | H]; rewrite H in *.
  - destruct IH as [Hfirst | Hrest]; [now left | now right; right].
  - destruct IH as [Hnext | Hrest]; [now right; left | now right; right].
Qed.

Theorem select_bucket_member bucket id :
  select_bucket bucket = Some id -> exists kind, In (id, kind) bucket.
Proof.
  destruct bucket as [| first rest]; [discriminate |].
  simpl. destruct (fold_left pick rest first) as [chosen kind] eqn:Hchoice.
  intros H; inversion H; subst. exists kind.
  pose proof (fold_pick_member rest first) as Hmember.
  rewrite Hchoice in Hmember. exact Hmember.
Qed.

(* A separate viewport uses one slot. A one-image observation uses a
   montage with the viewport as an additional tile in that same slot. *)
Definition history_slots (budget : N) : N :=
  if 1 <? budget then budget - 1 else budget.

Theorem history_slots_reserve_viewport budget :
  1 < budget -> history_slots budget + 1 = budget.
Proof.
  intros H. unfold history_slots.
  assert ((1 <? budget) = true) as -> by now apply N.ltb_lt.
  lia.
Qed.

(* First spread the history selection over time, then pack at most three
   selected assets into an image. The final viewport has its own place in
   every valid delivery plan. Its pixels come from the last frame group. *)
Fixpoint buckets {A : Type} (fuel : nat) (total slots previous : N) (items : list A)
  : list (list A) :=
  match fuel, items with
  | O, _ | _, [] => []
  | S remaining, _ =>
    let next := previous + 1 in
    let size := N.to_nat (next * total / slots - previous * total / slots) in
    firstn size items :: buckets remaining total slots next (skipn size items)
  end.

Definition split_evenly {A : Type} (slots : N) (items : list A) :=
  buckets (N.to_nat slots) (N.of_nat (length items)) slots 0 items.

Definition history_choices (slots : N) (kinds : list (N * image_kind)) : list N :=
  let limit := 3 * slots in
  if N.of_nat (length kinds) <=? limit then map fst kinds
  else flat_map (fun bucket =>
    match select_bucket bucket with Some id => [id] | None => [] end)
    (split_evenly limit kinds).

Inductive delivery := Combined (history : list N) | Separate (history : list (list N)).

Definition delivery_plan (budget : N) (kinds : list (N * image_kind)) : delivery :=
  let slots := history_slots budget in
  let chosen := history_choices slots kinds in
  if 1 <? budget
  then Separate (split_evenly (N.min slots (N.of_nat (length chosen))) chosen)
  else Combined chosen.

Lemma buckets_bound {A} fuel total slots previous (items : list A) :
  (length (buckets fuel total slots previous items) <= fuel)%nat.
Proof.
  revert previous items.
  induction fuel as [| fuel IH]; intros previous [| head rest]; simpl; try lia.
  apply le_n_S. apply IH.
Qed.

Theorem delivery_plan_budget budget kinds groups :
  delivery_plan budget kinds = Separate groups ->
  (N.of_nat (length groups) + 1 <= budget)%N.
Proof.
  unfold delivery_plan.
  destruct (1 <? budget) eqn:Hbudget; [| discriminate].
  intro H. inversion H; subst groups; clear H.
  apply N.ltb_lt in Hbudget.
  pose proof (history_slots_reserve_viewport budget Hbudget) as Hslots.
  unfold split_evenly.
  set (chosen := history_choices (history_slots budget) kinds) in *.
  set (slots := N.min (history_slots budget) (N.of_nat (length chosen))) in *.
  pose proof (buckets_bound (N.to_nat slots) (N.of_nat (length chosen)) slots 0 chosen) as Hcount.
  assert (N.of_nat (length (buckets (N.to_nat slots) (N.of_nat (length chosen)) slots 0 chosen))
          <= slots) as Hbound.
  { apply Nat.compare_le_iff in Hcount.
    rewrite Nat2N.inj_compare, N2Nat.id in Hcount.
    exact Hcount. }
  assert (slots <= history_slots budget) by (unfold slots; apply N.le_min_l).
  lia.
Qed.

Theorem one_image_includes_viewport kinds :
  exists history, delivery_plan 1 kinds = Combined history.
Proof. eexists. reflexivity. Qed.
