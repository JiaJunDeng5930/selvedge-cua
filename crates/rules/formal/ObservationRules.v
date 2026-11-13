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

(* The capture adapter uses this threshold to protect local changes from
   scroll merging. Area is measured in pixels. *)
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

(* Each row describes the readable attachments starting at one evidence
   position. A candidate covers that position and [following] more.
   Its cost includes metadata attributable to the attachment. *)
Record candidate := Candidate { following : nat; estimated_tokens : N }.

Record packing := Packing {
  tokens : N;
  attachment_count : N;
  spans : list N
}.

Definition empty_packing := Packing 0 0 [].

Definition prepend (first : candidate) (suffix : packing) :=
  Packing (estimated_tokens first + tokens suffix)
          (N.succ (attachment_count suffix))
          (N.pos (Pos.of_succ_nat (following first)) :: spans suffix).

Definition no_more_expensive a b :=
  tokens a < tokens b \/ (tokens a = tokens b /\ attachment_count a <= attachment_count b).

Definition cheaper_or_equal a b :=
  if tokens a =? tokens b
  then attachment_count a <=? attachment_count b
  else tokens a <? tokens b.

Definition prefer (a b : option packing) :=
  match a, b with
  | Some first, Some second => if cheaper_or_equal first second then a else b
  | Some _, None => a
  | None, _ => b
  end.

Definition extend (first : candidate) (suffixes : list (option packing)) :=
  match nth_error suffixes (following first) with
  | Some (Some suffix) => Some (prepend first suffix)
  | _ => None
  end.

Fixpoint choose (row : list candidate) (suffixes : list (option packing)) :=
  match row with
  | [] => None
  | first :: rest => prefer (extend first suffixes) (choose rest suffixes)
  end.

(* The previous table supplies the best packing of every suffix with
   one fewer available attachment. The final cell covers empty evidence. *)
Fixpoint advance (rows : list (list candidate)) (previous : list (option packing)) :=
  match rows, previous with
  | row :: rest, _ :: suffixes => choose row suffixes :: advance rest suffixes
  | _, _ => [Some empty_packing]
  end.

Fixpoint packings (budget : nat) (rows : list (list candidate)) :=
  match budget with
  | O => map (fun _ => None) rows ++ [Some empty_packing]
  | S remaining => advance rows (packings remaining rows)
  end.

Definition pack_images (budget : N) (rows : list (list candidate)) : option (list N) :=
  match hd None (packings (N.to_nat budget) rows) with
  | Some plan => Some (spans plan)
  | None => None
  end.

(* Feasibility follows the ordered evidence independently of the
   algorithm's tables. A candidate may consume only existing positions. *)
Inductive feasible : nat -> list (list candidate) -> packing -> Prop :=
  | finish budget : feasible budget [] empty_packing
  | attach budget row rest first suffix :
      In first row ->
      (following first <= length rest)%nat ->
      feasible budget (skipn (following first) rest) suffix ->
      feasible (S budget) (row :: rest) (prepend first suffix).

Definition best (valid : packing -> Prop) (choice : option packing) : Prop :=
  match choice with
  | Some selected => valid selected /\ forall other, valid other -> no_more_expensive selected other
  | None => forall other, ~ valid other
  end.

Lemma comparison_contract a b :
  if cheaper_or_equal a b then no_more_expensive a b else no_more_expensive b a.
Proof.
  unfold cheaper_or_equal, no_more_expensive.
  destruct (N.eqb_spec (tokens a) (tokens b)) as [equal | different].
  - destruct (N.leb_spec0 (attachment_count a) (attachment_count b)); right; lia.
  - destruct (N.ltb_spec0 (tokens a) (tokens b)); left; lia.
Qed.

Lemma cost_order_transitive a b c :
  no_more_expensive a b -> no_more_expensive b c -> no_more_expensive a c.
Proof. unfold no_more_expensive. lia. Qed.

Lemma prepend_preserves_cost_order first a b :
  no_more_expensive a b -> no_more_expensive (prepend first a) (prepend first b).
Proof. unfold no_more_expensive, prepend. simpl. lia. Qed.

Lemma best_equivalent (valid other : packing -> Prop) choice :
  (forall plan, valid plan <-> other plan) ->
  best valid choice -> best other choice.
Proof. destruct choice; simpl; firstorder. Qed.

Lemma prefer_best (valid_a valid_b : packing -> Prop) a b :
  best valid_a a ->
  best valid_b b ->
  best (fun plan => valid_a plan \/ valid_b plan) (prefer a b).
Proof.
  destruct a as [a|], b as [b|]; simpl.
  - pose proof (comparison_contract a b) as order.
    intros [valid_left lower_left] [valid_right lower_right].
    destruct (cheaper_or_equal a b); simpl.
    + split; [now left |].
      intros other [valid | valid]; [now apply lower_left |].
      eapply cost_order_transitive; [exact order | now apply lower_right].
    + split; [now right |].
      intros other [valid | valid]; [| now apply lower_right].
      eapply cost_order_transitive; [exact order | now apply lower_left].
  - firstorder.
  - firstorder.
  - firstorder.
Qed.

Fixpoint valid_table budget rows table : Prop :=
  match rows, table with
  | [], [choice] => best (feasible budget []) choice
  | _ :: rest, choice :: suffixes =>
      best (feasible budget rows) choice /\ valid_table budget rest suffixes
  | _, _ => False
  end.

Lemma table_length budget rows table :
  valid_table budget rows table -> length table = S (length rows).
Proof.
  revert table.
  induction rows as [|row rest IH]; intros [|choice suffixes]; simpl; try contradiction.
  - destruct suffixes; simpl; intuition.
  - intros [_ tail]. now rewrite (IH _ tail).
Qed.

Lemma table_lookup budget rows table offset choice :
  valid_table budget rows table ->
  nth_error table offset = Some choice ->
  best (feasible budget (skipn offset rows)) choice.
Proof.
  revert rows table.
  induction offset as [|offset IH]; intros [|row rest] [|current suffixes] table_ok at_offset;
    simpl in *; try contradiction.
  - destruct suffixes; try contradiction.
    injection at_offset as <-. exact table_ok.
  - injection at_offset as <-. tauto.
  - destruct suffixes; try contradiction. destruct offset; discriminate.
  - exact (IH rest suffixes (proj2 table_ok) at_offset).
Qed.

Definition fits budget rows first plan :=
  (following first <= length rows)%nat /\
  exists suffix, feasible budget (skipn (following first) rows) suffix /\ plan = prepend first suffix.

Lemma extend_best budget rows table first :
  valid_table budget rows table ->
  best (fits budget rows first) (extend first table).
Proof.
  intro table_ok.
  unfold extend.
  destruct (nth_error table (following first)) as [choice|] eqn:at_offset.
  - assert (within : (following first <= length rows)%nat).
    { pose proof (table_length _ _ _ table_ok) as count.
      assert (nth_error table (following first) <> None) as present by congruence.
      apply nth_error_Some in present.
      lia. }
    pose proof (table_lookup _ _ _ _ _ table_ok at_offset) as suffix_best.
    destruct choice as [suffix|]; simpl in *.
    + destruct suffix_best as [valid lower]. split.
      * split; eauto.
      * intros other [_ [alternative [valid_alternative ->]]].
        apply prepend_preserves_cost_order. now apply lower.
    + intros other [_ [alternative [valid _]]].
      now apply (suffix_best alternative).
  - intros other [within _].
    apply nth_error_None in at_offset.
    pose proof (table_length _ _ _ table_ok). lia.
Qed.

Lemma choose_best budget rows table row :
  valid_table budget rows table ->
  best (fun plan => exists first, In first row /\ fits budget rows first plan) (choose row table).
Proof.
  intro table_ok.
  induction row as [|first rest IH]; simpl choose.
  - simpl. firstorder.
  - apply (best_equivalent (fun plan =>
      fits budget rows first plan \/ (exists choice, In choice rest /\ fits budget rows choice plan))).
    + intros plan. simpl In. split.
      * intros [fits_first | [choice [available fits_choice]]].
        -- exists first. auto.
        -- exists choice. auto.
      * intros [choice [[equal | available] fits_choice]].
        -- subst choice. now left.
        -- right. eauto.
    + apply prefer_best; [now apply extend_best | exact IH].
Qed.

Lemma empty_packing_best budget :
  best (feasible budget []) (Some empty_packing).
Proof.
  split; [constructor |].
  intros other valid. inversion valid; subst.
  unfold no_more_expensive. right. split; [reflexivity | apply N.le_refl].
Qed.

Lemma advance_valid budget rows previous :
  valid_table budget rows previous -> valid_table (S budget) rows (advance rows previous).
Proof.
  revert previous.
  induction rows as [|row rest IH]; intros [|choice suffixes]; simpl; try contradiction.
  - intros _. apply empty_packing_best.
  - intros [_ suffixes_ok]. split.
    + apply (best_equivalent (fun plan => exists first, In first row /\ fits budget rest first plan)).
      * intros plan. split.
        -- intros [first [available [within [suffix [valid ->]]]]]. now constructor.
        -- intro valid.
           inversion valid as [|budget' row' rest' first suffix available within tail_valid]; subst.
           exists first. split; [exact available |].
           split; [exact within |]. exists suffix. auto.
      * now apply choose_best.
    + now apply IH.
Qed.

Lemma zero_budget rows :
  valid_table O rows (packings O rows).
Proof.
  induction rows as [|row rest IH]; simpl.
  - apply empty_packing_best.
  - split; [intros other valid; inversion valid | exact IH].
Qed.

Lemma packing_table_valid budget rows :
  valid_table budget rows (packings budget rows).
Proof.
  induction budget; [apply zero_budget | simpl; now apply advance_valid].
Qed.

Lemma feasible_coverage budget rows plan :
  feasible budget rows plan ->
  Forall (fun span => 0 < span) (spans plan) /\
  fold_right N.add 0 (spans plan) = N.of_nat (length rows) /\
  length (spans plan) = N.to_nat (attachment_count plan) /\
  (length (spans plan) <= budget)%nat.
Proof.
  intro valid.
  induction valid as [budget|budget row rest first suffix available within valid IH].
  - simpl. repeat split; try constructor; lia.
  - destruct IH as [positive [coverage [count bounded]]].
    cbn [prepend spans attachment_count length fold_right].
    change (N.pos (Pos.of_succ_nat (following first))) with (N.of_nat (S (following first))).
    rewrite coverage, length_skipn, <- Nat2N.inj_add, N2Nat.inj_succ.
    replace (S (following first) + (length rest - following first))%nat with (S (length rest)) by lia.
    repeat split; try lia.
    constructor; [rewrite Nat2N.inj_succ; lia | exact positive].
Qed.

Theorem pack_images_contract budget rows :
  match pack_images budget rows with
  | Some selected =>
      exists plan, spans plan = selected /\
        feasible (N.to_nat budget) rows plan /\
        (forall other, feasible (N.to_nat budget) rows other -> no_more_expensive plan other) /\
        Forall (fun span => 0 < span) selected /\
        fold_right N.add 0 selected = N.of_nat (length rows) /\
        (length selected <= N.to_nat budget)%nat
  | None => forall other, ~ feasible (N.to_nat budget) rows other
  end.
Proof.
  unfold pack_images.
  pose proof (packing_table_valid (N.to_nat budget) rows) as table_ok.
  destruct (packings (N.to_nat budget) rows) as [|choice suffixes] eqn:table.
  - pose proof (table_length _ _ _ table_ok). discriminate.
  - pose proof (table_lookup _ _ _ O choice table_ok eq_refl) as selected_best.
    simpl in selected_best |- *.
    destruct choice as [selected|]; simpl in selected_best.
    + destruct selected_best as [valid lower].
      destruct (feasible_coverage _ _ _ valid) as [positive [coverage [count bounded]]].
      eauto 10.
    + exact selected_best.
Qed.
