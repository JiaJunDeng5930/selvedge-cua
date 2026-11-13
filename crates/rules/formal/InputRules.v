From Stdlib Require Import NArith List Lia.
Import ListNotations.
Open Scope N_scope.

(* This is the input state owned by one desktop session. A physical key may
   have only one target while held. Its target remains part of the control so
   a later cleanup reaches the same application. *)
Inductive control :=
| Mouse (button : N)
| Key (code target : N).

Definition same_physical (left right : control) : bool :=
  match left, right with
  | Mouse a, Mouse b => N.eqb a b
  | Key a _, Key b _ => N.eqb a b
  | _, _ => false
  end.

Definition same_control (left right : control) : bool :=
  match left, right with
  | Mouse a, Mouse b => N.eqb a b
  | Key a pid_a, Key b pid_b => N.eqb a b && N.eqb pid_a pid_b
  | _, _ => false
  end.

Inductive transition :=
| Unchanged
| Press (input : control)
| Release (input : control).

Inductive outcome (A : Type) :=
| Accepted (value : A)
| OwnerRequired
| StaleReference
| ReferencesExhausted
| DestinationMismatch
| OutOfOrder
| AlreadyPressed
| NotPressed.
Arguments Accepted {A} _.
Arguments OwnerRequired {A}.
Arguments StaleReference {A}.
Arguments ReferencesExhausted {A}.
Arguments DestinationMismatch {A}.
Arguments OutOfOrder {A}.
Arguments AlreadyPressed {A}.
Arguments NotPressed {A}.

(* The most recently pressed control is first. This is also the release
   order, so a modifier pressed before a mouse button remains down through
   the button-up event. Removing a control preserves the remaining order. *)
Fixpoint remove_held (input : control) (held : list control) : list control :=
  match held with
  | [] => []
  | first :: rest =>
    if same_control input first then rest else first :: remove_held input rest
  end.

Definition apply_transition (held : list control) (action : transition)
  : outcome (list control) :=
  match action with
  | Unchanged => Accepted held
  | Press input =>
    if existsb (same_physical input) held then AlreadyPressed
    else Accepted (input :: held)
  | Release input =>
    if existsb (same_control input) held then Accepted (remove_held input held)
    else NotPressed
  end.

Record timed_step := Step { offset_ms : N; action : transition }.

(* Offsets are relative to the start of this invocation. Equal offsets
   preserve caller order, for example a simultaneous key chord. *)
Definition advance (previous : N) (held : list control) (next : timed_step)
  : outcome (list control) :=
  if N.leb previous (offset_ms next)
  then apply_transition held (action next)
  else OutOfOrder.

Fixpoint validate_plan (previous : N) (held : list control) (steps : list timed_step)
  : outcome (list control) :=
  match steps with
  | [] => Accepted held
  | next :: remaining =>
    match advance previous held next with
    | Accepted after => validate_plan (offset_ms next) after remaining
    | OutOfOrder => OutOfOrder
    | AlreadyPressed => AlreadyPressed
    | NotPressed => NotPressed
    | OwnerRequired => OwnerRequired
    | StaleReference => StaleReference
    | ReferencesExhausted => ReferencesExhausted
    | DestinationMismatch => DestinationMismatch
    end
  end.

(* The previous reference is the last ID ever allocated by this session.
   With a nonempty held list it is also the only live continuation ID.
   Using numbers up to 2^53-1 keeps the JavaScript round trip exact. *)
Definition max_reference : N := 9007199254740991.

Definition authorize (previous : N) (held : list control) (requested : option N)
  : outcome N :=
  match held, requested with
  | [], None =>
    if N.ltb previous max_reference then Accepted (N.succ previous)
    else ReferencesExhausted
  | [], Some _ => StaleReference
  | _ :: _, None => OwnerRequired
  | _ :: _, Some supplied =>
    if N.eqb previous supplied then Accepted previous else StaleReference
  end.

Definition releases (hold : bool) (held : list control) : list control :=
  if hold then [] else held.

(* The state here is a record of acknowledged dispatches. The host validates
   the full plan on a private copy before dispatch. It
   then applies each transition only after the platform acknowledges it.
   Cleanup similarly changes the state only after acknowledging each up. *)

Inductive physical_control :=
| MouseButton (button : N)
| KeyboardKey (code : N).

Definition physical (input : control) : physical_control :=
  match input with
  | Mouse button => MouseButton button
  | Key code _ => KeyboardKey code
  end.

Lemma same_physical_spec left right :
  same_physical left right = true <-> physical left = physical right.
Proof.
  destruct left, right; simpl; split; intros H; try discriminate;
    try (apply N.eqb_eq in H; subst; reflexivity);
    inversion H; apply N.eqb_refl.
Qed.

Lemma same_control_spec left right :
  same_control left right = true <-> left = right.
Proof.
  destruct left, right; simpl; try (split; discriminate).
  - rewrite N.eqb_eq. split; congruence.
  - rewrite Bool.andb_true_iff, !N.eqb_eq. split; [intros [-> ->]; reflexivity | intro H; inversion H; auto].
Qed.

Lemma physical_membership input held :
  existsb (same_physical input) held = true <-> In (physical input) (map physical held).
Proof.
  rewrite existsb_exists, in_map_iff.
  split.
  - intros [other [Hin Heq]]. exists other. split; [symmetry; now apply same_physical_spec | exact Hin].
  - intros [other [Heq Hin]]. exists other. split; [exact Hin | apply same_physical_spec; now symmetry].
Qed.

Lemma exact_membership input held :
  existsb (same_control input) held = true <-> In input held.
Proof.
  rewrite existsb_exists. split.
  - intros [other [Hin Heq]]. now apply same_control_spec in Heq; subst.
  - intros Hin. exists input. split; [exact Hin | now apply same_control_spec].
Qed.

Lemma remove_held_in input held other :
  In other (remove_held input held) -> In other held.
Proof.
  induction held as [| first rest IH]; simpl; [tauto |].
  destruct (same_control input first); simpl; intuition.
Qed.

Lemma remove_held_spec input held :
  NoDup (map physical held) ->
  NoDup (map physical (remove_held input held)) /\
  ~ In input (remove_held input held).
Proof.
  induction held as [| first rest IH]; simpl; intro Hunique.
  - split; [constructor | tauto].
  - inversion Hunique as [| ? ? Habsent Hrest]; subst.
    destruct (same_control input first) eqn:Heq.
    + apply same_control_spec in Heq. subst first.
      split; [exact Hrest | intros Hin; apply Habsent; now apply in_map].
    + destruct (IH Hrest) as [Hunique' Hout].
      split.
      * constructor; [| exact Hunique'].
        intro Hin. apply Habsent.
        apply in_map_iff in Hin as [other [Heq' Hin]].
        apply in_map_iff. exists other. split; [exact Heq' |].
        now apply (remove_held_in input).
      * intros [Hsame | Hin]; [| contradiction].
        subst first. assert (same_control input input = true) by (apply same_control_spec; reflexivity).
        congruence.
Qed.

(* Successful press and release have a positive effect on the recorded
   state. Their conditions use physical membership and exact membership
   respectively, so a key cannot be pressed again by changing its PID. *)
Theorem press_accepted input held :
  ~ In (physical input) (map physical held) <->
  apply_transition held (Press input) = Accepted (input :: held).
Proof.
  simpl. destruct (existsb (same_physical input) held) eqn:H.
  - apply physical_membership in H. split; [contradiction | discriminate].
  - split; [reflexivity | intros _ Hin; apply physical_membership in Hin; congruence].
Qed.

Theorem release_accepted input held :
  In input held <->
  apply_transition held (Release input) = Accepted (remove_held input held).
Proof.
  simpl. destruct (existsb (same_control input) held) eqn:H.
  - split; [reflexivity | intros _; now apply exact_membership].
  - split; [intros Hin; apply exact_membership in Hin; congruence | discriminate].
Qed.

Theorem apply_preserves_unique held change next :
  NoDup (map physical held) ->
  apply_transition held change = Accepted next ->
  NoDup (map physical next).
Proof.
  intros Hunique Hstep. destruct change as [| input | input]; simpl in Hstep.
  - now injection Hstep as <-.
  - destruct (existsb (same_physical input) held) eqn:Hin; try discriminate.
    injection Hstep as <-. constructor; [| exact Hunique].
    intros H. apply physical_membership in H. congruence.
  - destruct (existsb (same_control input) held); try discriminate.
    injection Hstep as <-. exact (proj1 (remove_held_spec input held Hunique)).
Qed.

Theorem advance_contract previous held next result :
  advance previous held next = Accepted result <->
  previous <= offset_ms next /\ apply_transition held (action next) = Accepted result.
Proof.
  unfold advance. destruct (N.leb previous (offset_ms next)) eqn:H.
  - apply N.leb_le in H. tauto.
  - apply N.leb_gt in H. split; [discriminate | intros [Hle _]; lia].
Qed.

Inductive ordered_dispatches : N -> list control -> list timed_step -> list control -> Prop :=
| PlanEnd previous held : ordered_dispatches previous held [] held
| PlanStep previous held next remaining after final :
  previous <= offset_ms next ->
  apply_transition held (action next) = Accepted after ->
  ordered_dispatches (offset_ms next) after remaining final ->
  ordered_dispatches previous held (next :: remaining) final.

Theorem validation_contract previous held steps final :
  validate_plan previous held steps = Accepted final <->
  ordered_dispatches previous held steps final.
Proof.
  revert previous held.
  induction steps as [| next remaining IH]; intros previous held; simpl.
  - split; intro H; [inversion H; constructor | now inversion H].
  - split.
    + destruct (advance previous held next) eqn:H; try discriminate.
      intros Hrest. apply advance_contract in H as [Horder Htransition].
      eapply PlanStep; [exact Horder | exact Htransition | now apply IH].
    + intro H. inversion H; subst.
      assert (advance previous held next = Accepted after) as ->.
      { apply advance_contract. auto. }
      now apply IH.
Qed.

Theorem validation_preserves_unique previous held steps final :
  NoDup (map physical held) ->
  validate_plan previous held steps = Accepted final ->
  NoDup (map physical final).
Proof.
  intros Hunique Hplan. apply validation_contract in Hplan.
  induction Hplan; [exact Hunique |].
  apply IHHplan. eapply apply_preserves_unique; eassumption.
Qed.

Theorem empty_session_allocates previous :
  previous < max_reference ->
  authorize previous [] None = Accepted (N.succ previous) /\
  previous < N.succ previous <= max_reference.
Proof.
  intro H. unfold authorize.
  assert (N.ltb previous max_reference = true) as -> by now apply N.ltb_lt.
  split; [reflexivity | lia].
Qed.

Theorem only_live_owner_can_continue previous first rest requested id :
  authorize previous (first :: rest) requested = Accepted id <->
  requested = Some previous /\ id = previous.
Proof.
  destruct requested as [request |]; simpl; [| split; [discriminate | intros [? _]; discriminate]].
  destruct (N.eqb previous request) eqn:Heq.
  - apply N.eqb_eq in Heq; subst request. split; [intros H; inversion H; auto | intros [_ ->]; reflexivity].
  - apply N.eqb_neq in Heq. split; [discriminate | intros [H _]; congruence].
Qed.

Theorem closed_reference_is_stale previous requested :
  authorize previous [] (Some requested) = StaleReference.
Proof. reflexivity. Qed.

(* Every acknowledged cleanup removes exactly its head. This witness also
   gives a valid way to end any held operation with no additional presses. *)
Theorem cleanup_releases_latest input rest :
  apply_transition (input :: rest) (Release input) = Accepted rest.
Proof.
  simpl. assert (same_control input input = true) as -> by now apply same_control_spec.
  reflexivity.
Qed.

Inductive dispatches : list control -> list transition -> list control -> Prop :=
| DispatchEnd held : dispatches held [] held
| DispatchNext held change next remaining final :
  apply_transition held change = Accepted next ->
  dispatches next remaining final ->
  dispatches held (change :: remaining) final.

Theorem finish_releases_all held :
  dispatches held (map Release (releases false held)) [].
Proof.
  induction held as [| input rest IH].
  - constructor.
  - simpl. eapply DispatchNext; [apply cleanup_releases_latest | exact IH].
Qed.

Theorem explicit_hold_preserves held :
  dispatches held (map Release (releases true held)) held.
Proof. constructor. Qed.

(* Only successful completion can retain controls. Cancellation and a failed
   send use the same cleanup order over the acknowledged state. *)
Definition finish_releases (hold failed : bool) (held : list control) :=
  releases (hold && negb failed) held.

Theorem failure_releases_all hold held :
  dispatches held (map Release (finish_releases hold true held)) [].
Proof. destruct hold; apply finish_releases_all. Qed.

Definition new_pointer_presses (held : list control) (buttons : list N) :=
  filter (fun button => negb (existsb (same_physical (Mouse button)) held)) buttons.

Theorem new_pointer_presses_contract held buttons button :
  In button (new_pointer_presses held buttons) <->
  In button buttons /\ ~ In (MouseButton button) (map physical held).
Proof.
  unfold new_pointer_presses. rewrite filter_In, Bool.negb_true_iff.
  split; intros [Hin H]; split; try exact Hin.
  - intros Hpresent.
    assert (existsb (same_physical (Mouse button)) held = true) by
      (apply physical_membership; exact Hpresent). congruence.
  - destruct (existsb (same_physical (Mouse button)) held) eqn:Hpresent; [|reflexivity].
    apply physical_membership in Hpresent. contradiction.
Qed.

(* A held operation belongs to a process and may also have a window bound
   to it. The keyboard can establish the process before the first mouse
   action binds a window. A continuation can add that window once. This
   prevents a gesture from switching windows or redirecting key releases. *)
Record destination := Destination { process : N; window : option N }.

Definition bind_destination (active : option destination) (next : destination)
  : outcome destination :=
  match active with
  | None => Accepted next
  | Some current =>
      if process current =? process next then
        match window current, window next with
        | None, _ => Accepted next
        | Some _, None => Accepted current
        | Some before, Some after =>
            if before =? after then Accepted current else DestinationMismatch
        end
      else DestinationMismatch
  end.

Theorem first_destination next : bind_destination None next = Accepted next.
Proof. reflexivity. Qed.

Theorem continuation_preserves_destination current next bound :
  bind_destination (Some current) next = Accepted bound ->
  process current = process next /\ process bound = process current /\
  (forall id, window current = Some id -> window bound = Some id).
Proof.
  destruct current as [pid current], next as [pid' next].
  cbn. destruct (pid =? pid') eqn:Hpid; [| discriminate].
  apply N.eqb_eq in Hpid. subst pid'.
  destruct current as [current|], next as [next|]; simpl;
    try (destruct (current =? next));
    intros H; try discriminate; inversion H; subst; simpl; intuition congruence.
Qed.
