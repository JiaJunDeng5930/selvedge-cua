From Stdlib Require Import List Arith Lia.
Import ListNotations.

(** Engineering requirements for Selvedge CUA.

    The subject is the interaction seen by an AI script and by the person
    using the same computer.  These are specifications and derivations;
    neither an input scheduler nor an image encoder is defined here.

    We model only task-relevant observations.  An application can change
    between observations even with no input.  The computer can accept an
    input before the application shows a response.  These two facts make
    a record of times and effects necessary.  They do not determine the
    number of functions, the compression algorithm, or the runtime.

    Dependency order: distinguishable task outcomes -> observation;
    timed intent and platform outcomes -> continuation and completion;
    observation + dispatch -> coordination; platform observations and
    effects -> background operation and portability. *)

(** If a user must answer a question from a result alone, the result
    must distinguish any two admissible cases with different answers.
    Conversely, a collision is a counterexample to any decoder, including
    one that can interpret image content. *)
Section Information.
  Context {World Report Answer : Type}.
  Variable admissible : World -> Prop.
  Variable report : World -> Report.
  Variable answer : World -> Answer.

  Definition answerable : Prop :=
    exists interpret : Report -> Answer,
      forall world, admissible world -> interpret (report world) = answer world.

  Theorem necessary_distinction :
    answerable -> forall a b, admissible a -> admissible b ->
      answer a <> answer b -> report a <> report b.
  Proof.
    intros [interpret valid] a b Ha Hb different same.
    apply different. rewrite <- (valid a Ha), <- (valid b Hb), same. reflexivity.
  Qed.
End Information.

(** Times denote observations or effects on a common monotonic timeline.
    Equal ticks are possible at finite resolution.  A frame time must
    describe the capture, not merely when image encoding finished.
    Capture and dispatch timestamp sources are an external boundary:
    their epochs, units and acquisition points must support this meaning.
    ScreenCaptureKit documents frame display time and geometry metadata:
    https://developer.apple.com/documentation/screencapturekit/scstreamframeinfo/displaytime
    This documents an available measurement, not the timestamp semantics
    of every capture API or adapter. *)
Definition Tick := nat.

Record Timed (A : Type) := {
  at_tick : Tick;
  value : A
}.
Arguments at_tick {A} _.
Arguments value {A} _.

Definition current_view {A} (samples : list A) (final : A) : Prop :=
  exists earlier, samples = earlier ++ [final].

Definition chronological {Event} (events : list (Timed Event)) : Prop :=
  forall pre a mid b post,
    events = pre ++ a :: mid ++ b :: post -> at_tick a <= at_tick b.

(** The collector establishes the target, clock interval and order
    once for each observation. Encoders consume this same batch. *)
Definition capture_batch {Sample Target} (target_of : Sample -> Target)
    target start finish (frames : list (Timed Sample)) : Prop :=
  frames <> [] /\ chronological frames /\
  Forall (fun f => target_of (value f) = target /\ start <= at_tick f <= finish) frames.

(** A report must carry the selected evidence at a size the AI can use.
    An image consumer can shrink a whole attachment before the model
    sees it. Geometry and detail sizes below are measured after that
    transformation, including raster rounding. A readable minimum is
    specific to the task and consumer: if the model misses a required
    label or control at that size, its calibration is refuted.

    Compare layouts that carry the same ordered evidence. Count image
    tokens using the consumer's rule on the actual attachments. Some
    models charge a fixed amount per image plus a tile cost; others
    use patch accounting. Neither the encoder nor the layout supplies
    its own price.
    https://developers.openai.com/api/docs/guides/images-vision *)
Module ImageLayout.
  Record Rectangle := {
    left : nat; top : nat; width : nat; height : nat
  }.

  Record Detail := {
    source_detail_region : Rectangle;
    minimum_width : nat; minimum_height : nat
  }.

  Record Tile (Evidence : Type) := {
    source : Evidence;
    region : Rectangle
  }.
  Arguments source {Evidence} _.
  Arguments region {Evidence} _.

  Record Attachment (Evidence Image : Type) := {
    image : Image;
    tiles : list (Tile Evidence)
  }.
  Arguments image {Evidence Image} _.
  Arguments tiles {Evidence Image} _.

  Definition layout_evidence {Evidence Image} (attachments : list (Attachment Evidence Image)) :=
    flat_map (fun attachment => map source (tiles attachment)) attachments.

  (** Consumers supply dimensions and token costs. Rendering and
      measurement are independent observations of the decoded image,
      in the same coordinate space. A successful rendering represents
      the entire requested evidence in its region and preserves its
      provenance and time metadata. Measuring a missing detail fails. *)
  Record Profile (Evidence Image : Type) := {
    image_extent : Image -> nat * nat;
    represents : Evidence -> Image -> Rectangle -> Prop;
    required_details : Evidence -> list Detail;
    measure_detail : Evidence -> Image -> Rectangle -> Detail -> option (nat * nat);
    image_tokens : Image -> nat
  }.
  Arguments image_extent {Evidence Image} _ _.
  Arguments represents {Evidence Image} _ _ _ _.
  Arguments required_details {Evidence Image} _ _.
  Arguments measure_detail {Evidence Image} _ _ _ _ _.
  Arguments image_tokens {Evidence Image} _ _.

  Section Layouts.
    Context {Evidence Image : Type}.
    Variable profile : Profile Evidence Image.
    Local Abbreviation Layout := (list (Attachment Evidence Image)).

    Definition readable (im : Image) (tile : Tile Evidence) (detail : Detail) : Prop :=
      exists w h, measure_detail profile (source tile) im (region tile) detail = Some (w,h) /\
        minimum_width detail <= w /\ minimum_height detail <= h.

    Definition valid_tile im (tile : Tile Evidence) : Prop :=
      let r := region tile in
      let '(w,h) := image_extent profile im in
      0 < width r /\ 0 < height r /\ left r + width r <= w /\ top r + height r <= h /\
      represents profile (source tile) im r /\
      Forall (readable im tile) (required_details profile (source tile)).

    Definition valid_attachment (attachment : Attachment Evidence Image) : Prop :=
      tiles attachment <> [] /\ Forall (valid_tile (image attachment)) (tiles attachment).

    Definition usable budget (evidence : list Evidence) (layout : Layout) : Prop :=
      length layout <= budget /\ layout_evidence layout = evidence /\
      Forall valid_attachment layout.

    Definition cost (layout : Layout) :=
      fold_right (fun attachment sum => image_tokens profile (image attachment) + sum) 0 layout.

    Definition no_more_expensive (a b : Layout) : Prop :=
      cost a < cost b \/ (cost a = cost b /\ length a <= length b).

    Definition optimal budget evidence (layout : Layout) : Prop :=
      usable budget evidence layout /\
      forall alternative, usable budget evidence alternative ->
        no_more_expensive layout alternative.

    Theorem readable_packing_rules_out_separate budget evidence (separate packed : Layout) :
      usable budget evidence packed ->
      length packed < length separate -> cost packed <= cost separate ->
      ~ optimal budget evidence separate.
    Proof.
      intros fits fewer cheaper [_ best].
      specialize (best packed fits). unfold no_more_expensive in best. lia.
    Qed.

    Theorem missing_or_unreadable_detail_rules_out_packing budget evidence
        (layout : Layout) attachment tile detail :
      In attachment layout -> In tile (tiles attachment) ->
      In detail (required_details profile (source tile)) ->
      ~ readable (image attachment) tile detail ->
      ~ usable budget evidence layout.
    Proof.
      intros Ha Ht Hd unreadable [_ [_ accepted]].
      rewrite Forall_forall in accepted. specialize (accepted _ Ha).
      destruct accepted as [_ accepted].
      rewrite Forall_forall in accepted. specialize (accepted _ Ht).
      unfold valid_tile in accepted.
      destruct (image_extent profile (image attachment)) as [w h].
      destruct accepted as [_ [_ [_ [_ [_ accepted]]]]].
      rewrite Forall_forall in accepted. auto.
    Qed.

    Definition variable_cost (charge : Image -> nat) (layout : Layout) :=
      fold_right (fun attachment sum => charge (image attachment) + sum) 0 layout.

    Lemma cost_with_fixed_charge base charge :
      (forall im, image_tokens profile im = base + charge im) ->
      forall layout, cost layout = base * length layout + variable_cost charge layout.
    Proof.
      intros charge_rule layout. induction layout as [|attachment rest IH]; simpl; [lia|].
      rewrite charge_rule, IH. lia.
    Qed.

    (** The variable cost can rise after packing, for instance because
        of padding or tile rounding. The fixed charge saved must cover
        that increase; equal total cost favors fewer attachments. *)
    Theorem packing_cost_threshold base charge (separate packed : Layout) :
      (forall im, image_tokens profile im = base + charge im) ->
      length packed <= length separate ->
      (cost packed <= cost separate <->
       variable_cost charge packed <= variable_cost charge separate +
         base * (length separate - length packed)).
    Proof.
      intros rule count. rewrite !(cost_with_fixed_charge _ _ rule).
      rewrite Nat.mul_sub_distr_l. nia.
    Qed.
  End Layouts.

  Definition delivered {Evidence Image} (profile : Profile Evidence Image)
      budget (accepts : list Evidence -> Prop) (layout : list (Attachment Evidence Image)) : Prop :=
    accepts (layout_evidence layout) /\
    optimal profile budget (layout_evidence layout) layout.
  (** Two 512x512 frames give a concrete, falsifiable cost comparison.
      The GPT-4.1 high-detail rule charges 85 tokens per image plus
      170 per 512x512 tile. These sizes are within the documented
      preprocessing bounds, so a horizontal join needs two tiles
      without resizing either frame. The glyph threshold below is a
      fixture parameter, not a general minimum for model legibility. *)
  Module LayoutExample.
    Inductive Frame := A | B.
    Inductive Image := Separate (frame : Frame) | Joined | Shrunk.

    Definition extent im :=
      match im with Separate _ => (512,512) | Joined => (1024,512) | Shrunk => (512,256) end.

    Definition glyph := {| source_detail_region := {| left := 24; top := 16; width := 12; height := 12 |};
                          minimum_width := 8; minimum_height := 8 |}.
    Definition square x side := {| left := x; top := 0; width := side; height := side |}.

    Definition frame_region im frame :=
      match im, frame with
      | Separate A, A | Separate B, B => Some (square 0 512)
      | Joined, A => Some (square 0 512)
      | Joined, B => Some (square 512 512)
      | Shrunk, A => Some (square 0 256)
      | Shrunk, B => Some (square 256 256)
      | _, _ => None
      end.

    Definition example_profile : Profile Frame Image :=
      {| image_extent := extent;
         represents := fun frame im r => frame_region im frame = Some r;
         required_details := fun _ => [glyph];
         measure_detail := fun _ im _ _ =>
           match im with Shrunk => Some (6,6) | _ => Some (12,12) end;
         image_tokens := fun im => match im with Joined => 425 | _ => 255 end |}.

    Definition tile frame r : Tile Frame := {| source := frame; region := r |}.
    Definition attachment im ts : Attachment Frame Image := {| image := im; tiles := ts |}.
    Definition separate := [attachment (Separate A) [tile A (square 0 512)];
                            attachment (Separate B) [tile B (square 0 512)]].
    Definition joined := [attachment Joined [tile A (square 0 512); tile B (square 512 512)]].
    Definition shrunk := [attachment Shrunk [tile A (square 0 256); tile B (square 256 256)]].

    Lemma glyph_is_readable im tile :
      im <> Shrunk -> readable example_profile im tile glyph.
    Proof. destruct im; intros different; [| |contradiction]; exists 12, 12; cbn; repeat split; lia. Qed.

    Lemma separate_images_are_usable : usable example_profile 2 [A; B] separate.
    Proof.
      repeat (first [apply Forall_nil | apply Forall_cons | split]);
        cbn in *; try lia; try discriminate; try reflexivity;
        apply glyph_is_readable; discriminate.
    Qed.

    Lemma joined_image_is_usable : usable example_profile 2 [A; B] joined.
    Proof.
      repeat (first [apply Forall_nil | apply Forall_cons | split]);
        cbn in *; try lia; try discriminate; try reflexivity;
        apply glyph_is_readable; discriminate.
    Qed.

    Example readable_join_saves_one_fixed_charge :
      cost example_profile separate = cost example_profile joined + 85 /\
      ~ optimal example_profile 2 [A; B] separate.
    Proof.
      split; [reflexivity |].
      apply (readable_packing_rules_out_separate _ _ _ _ joined);
        [exact joined_image_is_usable | simpl; lia | simpl; lia].
    Qed.

    Example unusable_join_leaves_a_readable_separate_option :
      ~ usable example_profile 2 [A; B] shrunk /\ usable example_profile 2 [A; B] separate.
    Proof.
      split; [|exact separate_images_are_usable].
      eapply (missing_or_unreadable_detail_rules_out_packing _ _ _ _
        (attachment Shrunk [tile A (square 0 256); tile B (square 256 256)])
        (tile A (square 0 256)) glyph); try (simpl; auto).
      intros (w & h & measured & width_ok & _). cbn in *. inversion measured. lia.
    Qed.
  End LayoutExample.
End ImageLayout.

Record ObservationWindow (Image : Type) := {
  requested_at : Tick;
  captured : list (Timed Image);
  ready_at : Tick;
  delivered_at : Tick
}.
Arguments captured {Image} _.
Arguments ready_at {Image} _.
Arguments delivered_at {Image} _.

Definition valid_window {Image} (window : ObservationWindow Image) : Prop :=
  exists first rest,
    captured window = first :: rest /\ chronological (captured window) /\
    requested_at Image window <= at_tick first <= ready_at window /\
    ready_at window <= delivered_at window /\
    Forall (fun f => at_tick f <= delivered_at window) (captured window).

Section Screen.
  Context {Target Raster LocalPoint DesktopPoint : Type}.

  (** A capture's identity includes its target and geometry.  Equal
      pixels alone need not imply the same point is actionable.  Desktop
      coordinates denote the operation coordinate system, including
      target and display geometry when needed for correct routing. *)
  Record Capture := {
    capture_target : Target;
    pixels : Raster;
    desktop_point : LocalPoint -> option DesktopPoint
  }.
  Definition Frame := Timed Capture.

  Definition captured_for := capture_batch capture_target.

  Definition one_shot (frame : Frame) (frames : list Frame) : Prop :=
    frames = [frame].

  Theorem one_shot_preserves_capture target start finish frame frames :
    captured_for target start finish frames -> one_shot frame frames ->
    capture_target (value frame) = target /\ start <= at_tick frame <= finish.
  Proof.
    intros [_ [_ H]] ->. now inversion H.
  Qed.

  (** A current picture is the last actual capture. Historical images
      may still be useful after their coordinates cease to be actionable. *)
  Theorem current_view_not_earlier (frames : list Frame) early current :
    chronological frames -> In early frames -> current_view frames current ->
    at_tick early <= at_tick current.
  Proof.
    intros ordered seen [prefix ->].
    apply in_app_or in seen. destruct seen as [seen | [<- | []]]; [|lia].
    apply in_split in seen. destruct seen as (pre & post & ->).
    apply (ordered pre early post current []); now rewrite <- app_assoc.
  Qed.

  (** Locating a point in a delivered image requires a source point and
      its actual capture.  This is the contract for any rescaling,
      composition or position metadata; layout coordinates alone lack
      the capture's time and coordinate transform. *)
  Definition point_evidence (frames : list Frame) (tick : Tick)
      (source : Frame) (point : LocalPoint) (destination : DesktopPoint) : Prop :=
    In source frames /\ tick = at_tick source /\
    desktop_point (value source) point = Some destination.

  Definition delivered_point {Evidence Image}
      (source_point : Evidence -> Image -> ImageLayout.Rectangle ->
                      LocalPoint -> option (Frame * LocalPoint))
      (frames : list Frame) (layout : list (ImageLayout.Attachment Evidence Image))
      attachment tile displayed tick frame local destination : Prop :=
    In attachment layout /\ In tile (ImageLayout.tiles attachment) /\
    source_point (ImageLayout.source tile) (ImageLayout.image attachment)
      (ImageLayout.region tile) displayed = Some (frame, local) /\
    point_evidence frames tick frame local destination.

  (** The point inverse is established by the image renderer. A tile's
      apparent position cannot replace the capture-time desktop transform. *)
  Theorem delivered_position_has_capture_provenance {Evidence Image}
      (source_point : Evidence -> Image -> ImageLayout.Rectangle ->
                      LocalPoint -> option (Frame * LocalPoint))
      frames layout attachment tile displayed tick frame local destination :
    delivered_point source_point frames layout attachment tile displayed tick frame local destination ->
    In frame frames /\ tick = at_tick frame /\
    desktop_point (value frame) local = Some destination.
  Proof. unfold delivered_point, point_evidence. tauto. Qed.

  Theorem geometry_matters (a b : Frame) point x y :
    desktop_point (value a) point = Some x ->
    desktop_point (value b) point = Some y -> x <> y -> value a <> value b.
  Proof. intros Ha Hb Hxy same. rewrite same, Hb in Ha. congruence. Qed.

  (** An interval observer may have to answer whether a visible
      state occurred between two captures of the same state.
      Let A and B be distinguishable full captures, including when
      their only difference is in a small region. *)
  Theorem endpoints_lose_a_transient (a b : Capture) (Report : Type)
      (encode : list Capture -> Report) :
    a <> b -> encode [a; a] = encode [a; b; a] ->
    ~ answerable (fun _ => True) encode (fun h => In b h).
  Proof.
    intros different same [interpret valid].
    pose proof (valid [a; a] I) as Ha.
    pose proof (valid [a; b; a] I) as Hb.
    rewrite same, Hb in Ha.
    assert (In b [a; a]) as impossible by (rewrite <- Ha; now right; left).
    simpl in impossible. tauto.
  Qed.

  (** For this task, the caller's possible decisions identify which
      captures can be represented together. Equal decision evidence is
      transitive and must include relevant coordinate mappings. It can
      ignore irrelevant pixel noise without equating distinguishable
      controls or states. Obtaining an adequate observation of those
      differences is a platform and task boundary. *)
  Definition decision_equivalent {Answer} (decide : Capture -> Answer)
      (a b : Capture) : Prop := decide a = decide b.

  (** One interval records a full representative and the first and last
      sample times. Equal samples extend the current interval. A later
      recurrence after a changed state starts a fresh interval. *)
  Record FullInterval := {
    first_sample : Frame;
    representative : Frame
  }.

  Inductive full_history (same : Capture -> Capture -> Prop) :
      list Frame -> list FullInterval -> Prop :=
  | single_sample : forall f,
      full_history same [f] [{| first_sample := f; representative := f |}]
  | unchanged_sample : forall f next rest interval history,
      same (value f) (value next) ->
      full_history same (next :: rest) (interval :: history) ->
      full_history same (f :: next :: rest)
        ({| first_sample := f; representative := representative interval |} :: history)
  | changed_sample : forall f next rest history,
      ~ same (value f) (value next) ->
      full_history same (next :: rest) history ->
      full_history same (f :: next :: rest)
        ({| first_sample := f; representative := f |} :: history).

  (** The current viewport must be the last actual capture and its
      evidence must be part of the same readable, economical report.
      Geometry here is the geometry at capture time. The application
      may move by the time the script uses the coordinates. *)
  Definition complete_evidence {Answer} (decide : Capture -> Answer)
      (frames : list Frame) final (evidence : list FullInterval) : Prop :=
    full_history (decision_equivalent decide) frames evidence /\ current_view frames final /\
    exists earlier last, evidence = earlier ++ [last] /\ representative last = final.

  Definition complete_screen_report {Image Answer}
      (profile : ImageLayout.Profile FullInterval Image) (decide : Capture -> Answer)
      target start finish budget frames final layout : Prop :=
    captured_for target start finish frames /\
    ImageLayout.delivered profile budget (complete_evidence decide frames final) layout.

  (** The distinct local state must be visible as an entire capture.
      Here A and B have different answers to a task-relevant question;
      for a localized change that must be retained, its visible state
      is itself part of the answer, even when the changed area is small.
      The five timestamps in the source samples remain available to
      the model; the output spans are [t1,t3], [t4,t4], [t5,t5]. *)
  Definition interval (first last : Frame) : FullInterval :=
    {| first_sample := first; representative := last |}.

  Lemma full_history_single same f result :
    full_history same [f] result -> result = [interval f f].
  Proof. now inversion 1. Qed.

  Lemma full_history_unchanged same f next rest result :
    same (value f) (value next) ->
    full_history same (f :: next :: rest) result ->
    exists prior history,
      full_history same (next :: rest) (prior :: history) /\
      result = interval f (representative prior) :: history.
  Proof. intros equivalent H. inversion H; subst; try contradiction. eauto. Qed.

  Lemma full_history_changed same f next rest result :
    ~ same (value f) (value next) ->
    full_history same (f :: next :: rest) result ->
    exists history, full_history same (next :: rest) history /\
      result = interval f f :: history.
  Proof. intros different H. inversion H; subst; try contradiction. eauto. Qed.

  Theorem transient_keeps_full_a_b_a {Image Answer}
      (profile : ImageLayout.Profile FullInterval Image) (decide : Capture -> Answer)
      a1 a2 a3 b a5 target start finish budget final layout :
    decision_equivalent decide (value a1) (value a2) ->
    decision_equivalent decide (value a2) (value a3) ->
    decision_equivalent decide (value a3) (value a5) ->
    ~ decision_equivalent decide (value a3) (value b) ->
    complete_screen_report profile decide target start finish budget [a1; a2; a3; b; a5] final layout ->
    ImageLayout.layout_evidence layout = [interval a1 a3; interval b b; interval a5 a5].
  Proof.
    intros H12 H23 H35 Hd [_ [[H _] _]].
    destruct (full_history_unchanged (decision_equivalent decide) _ _ _ _ H12 H) as (u & uh & Hu & ->).
    destruct (full_history_unchanged (decision_equivalent decide) _ _ _ _ H23 Hu) as (v & vh & Hv & EqV).
    destruct (full_history_changed (decision_equivalent decide) _ _ _ _ Hd Hv) as (w & Hw & EqW).
    assert (~ decision_equivalent decide (value b) (value a5)) as Hb.
    { unfold decision_equivalent in *. congruence. }
    destruct (full_history_changed (decision_equivalent decide) _ _ _ _ Hb Hw) as (z & Hz & ->).
    rewrite (full_history_single (decision_equivalent decide) _ _ Hz) in EqW.
    inversion EqW; subst.
    inversion EqV; subst. reflexivity.
  Qed.

  Theorem transient_evidence_is_feasible {Answer} (decide : Capture -> Answer) a1 a2 a3 b a5 :
    decision_equivalent decide (value a1) (value a2) ->
    decision_equivalent decide (value a2) (value a3) ->
    decision_equivalent decide (value a3) (value a5) ->
    ~ decision_equivalent decide (value a3) (value b) ->
    complete_evidence decide [a1; a2; a3; b; a5] a5 [interval a1 a3; interval b b; interval a5 a5].
  Proof.
    intros H12 H23 H35 Hd. split.
    - eapply unchanged_sample with (interval := interval a2 a3); [exact H12 |].
      eapply unchanged_sample with (interval := interval a3 a3); [exact H23 |].
      apply changed_sample; [exact Hd |].
      apply changed_sample; [unfold decision_equivalent in *; congruence | apply single_sample].
    - split.
      + now exists [a1; a2; a3; b].
      + exists [interval a1 a3; interval b b], (interval a5 a5). split; reflexivity.
  Qed.

  (** Animation can omit intervening frames if the evidence still
      answers the task's motion question. Important moments are given
      by that question, not by the position in a sampling bucket.
      The timestamped samples may be wrapped in a different layout. *)
  Inductive in_order {A} : list A -> list A -> Prop :=
  | ordered_nil : in_order [] []
  | ordered_keep : forall x kept source,
      in_order kept source -> in_order (x :: kept) (x :: source)
  | ordered_skip : forall x kept source,
      in_order kept source -> in_order kept (x :: source).

  Definition animation_evidence (important : Frame -> Prop) frames final evidence : Prop :=
    in_order evidence frames /\
    (forall f, In f frames -> important f -> In f evidence) /\
    current_view frames final /\ current_view evidence final.

  Definition animation_report {Image} (profile : ImageLayout.Profile Frame Image)
      important target start finish budget frames final layout : Prop :=
    captured_for target start finish frames /\
    ImageLayout.delivered profile budget (animation_evidence important frames final) layout.

  Theorem motion_evidence_cannot_omit_an_important_capture {Image}
      (profile : ImageLayout.Profile Frame Image) important target start finish budget frames final layout f :
    animation_report profile important target start finish budget frames final layout ->
    In f frames -> important f -> In f (ImageLayout.layout_evidence layout).
  Proof. intros [_ [[_ [keep _]] _]]. exact (keep f). Qed.
End Screen.

(** Aligned scrolling content is a union of observed locations.
    Source points describe the exact frame, geometry, and capture time.
    A stationary overlay or a changing pixel can invalidate a proposed
    alignment. Other evidence can be delivered alongside the scroll
    image because it answers a different question. *)
Section Scroll.
  Context {Sample Target LocalPoint ContentPoint Pixel : Type}.
  Local Abbreviation SampleFrame := (Timed Sample).
  Variable target_of : Sample -> Target.
  Variable read_pixel : SampleFrame -> LocalPoint -> option Pixel.
  Variable content_point : SampleFrame -> LocalPoint -> option ContentPoint.

  Definition observed_pixel (frames : list SampleFrame) point pixel : Prop :=
    exists frame local, In frame frames /\
      content_point frame local = Some point /\ read_pixel frame local = Some pixel.

  Definition mosaic_of frames (mosaic : ContentPoint -> option Pixel) : Prop :=
    forall point pixel, mosaic point = Some pixel <-> observed_pixel frames point pixel.

  Theorem inconsistent_overlap_refutes_alignment frames mosaic point a b :
    mosaic_of frames mosaic ->
    observed_pixel frames point a -> observed_pixel frames point b -> a = b.
  Proof.
    intros correct A B.
    apply (proj2 (correct point a)) in A. apply (proj2 (correct point b)) in B.
    congruence.
  Qed.

  Theorem mosaic_has_a_source frames mosaic point pixel :
    mosaic_of frames mosaic -> mosaic point = Some pixel ->
    exists frame local, In frame frames /\
      content_point frame local = Some point /\ read_pixel frame local = Some pixel.
  Proof. intros correct. exact (proj1 (correct point pixel)). Qed.

  (** The scroll layout, full-frame evidence of local changes, and the
      true final viewport share one budget. Metadata attaches a scroll
      tile to its source observations and a full tile to one full frame.
      The latter is also the coordinate source for the final viewport. *)
  Inductive Tile := Full (frame : SampleFrame) | Mosaic (content : ContentPoint -> option Pixel).

  Definition mixed_evidence (required_full : SampleFrame -> Prop) frames scroll_samples
      (mosaic : ContentPoint -> option Pixel) final evidence : Prop :=
    scroll_samples <> [] /\ in_order scroll_samples frames /\
    (exists point pixel, mosaic point = Some pixel) /\ mosaic_of scroll_samples mosaic /\
    In (Mosaic mosaic) evidence /\
    Forall (fun tile => match tile with
      | Full frame => In frame frames
      | Mosaic pixels => pixels = mosaic
      end) evidence /\
    in_order (flat_map (fun tile => match tile with
      | Full frame => [frame] | Mosaic _ => [] end) evidence) frames /\
    (forall frame, In frame frames -> required_full frame -> In (Full frame) evidence) /\
    current_view frames final /\ current_view evidence (Full final).

  (** The obligation to retain a localized change is decided by the
      observation task before layout selection. Mixing it with a
      scroll does not discharge that obligation. *)
  Definition mixed_report {Image} (profile : ImageLayout.Profile Tile Image)
      required_full target start finish budget frames scroll_samples mosaic final layout : Prop :=
    capture_batch target_of target start finish frames /\
    ImageLayout.delivered profile budget
      (mixed_evidence required_full frames scroll_samples mosaic final) layout.

  Theorem mixed_report_cannot_omit_required_local_frame {Image}
      (profile : ImageLayout.Profile Tile Image) required_full target start finish budget
      frames scroll_samples mosaic final layout frame :
    mixed_report profile required_full target start finish budget frames scroll_samples mosaic final layout ->
    In frame frames -> required_full frame -> In (Full frame) (ImageLayout.layout_evidence layout).
  Proof. unfold mixed_report, ImageLayout.delivered, mixed_evidence. firstorder. Qed.
End Scroll.

(** Time is part of an input request. A schedule records an intended
    offset from action start and a payload. The platform can issue late or
    fail, so a report separately records what was actually issued and
    when. Arbitrary continuous curves cannot be recovered from finitely
    many events; the position function specifies what to sample, and
    the required application-level check is one continuous undoable
    stroke. The model does not assume a sampling rate or timer precision. *)
Definition Intent (Event : Type) := list (Timed Event).

Definition due (start : Tick) {Event} (planned : Timed Event) : Tick :=
  start + at_tick planned.

Theorem different_offsets_have_different_due_times {Event} start
    (a b : Timed Event) :
  at_tick a <> at_tick b -> due start a <> due start b.
Proof. unfold due. lia. Qed.

(** A successful dispatch accounts for every planned payload in order.
    On interruption/failure only an acknowledged prefix may already have
    been issued. No theorem equates platform dispatch with app response.
    The clock comparison does not impose a deadline on the OS. *)
Inductive DispatchResult (Event : Type) :=
| issued (events : list (Timed Event))
| interrupted (events : list (Timed Event)).
Arguments issued {Event} _.
Arguments interrupted {Event} _.

(** The caller's clock and the platform need a shared timing contract.
    Never issue before the due time. The lateness allowed for a task is
    a parameter, because clicks and holds can cross application-specific
    timing thresholds. Explicit limits constrain success, not the OS. *)
Definition scheduled_pair {Event} start slack (planned sent : Timed Event) : Prop :=
  value sent = value planned /\ due start planned <= at_tick sent <= due start planned + slack.

Definition dispatch_valid {Event} start slack (plan : Intent Event)
    (result : DispatchResult Event) : Prop :=
  match result with
  | issued sent => chronological sent /\ Forall2 (scheduled_pair start slack) plan sent
  | interrupted sent => chronological sent /\
      exists issued_plan pending, plan = issued_plan ++ pending /\
        Forall2 (scheduled_pair start slack) issued_plan sent
  end.

Theorem successful_dispatch_cannot_omit_events {Event} start slack
    (plan : Intent Event) sent :
    dispatch_valid start slack plan (issued sent) -> length sent = length plan.
Proof. intros [_ H]. induction H; simpl; congruence. Qed.

Theorem schedule_separation_survives_dispatch {Event} start slack
    (early late first second : Timed Event) :
    scheduled_pair start slack early first -> scheduled_pair start slack late second ->
    at_tick early + slack < at_tick late -> at_tick first < at_tick second.
Proof. unfold scheduled_pair, due. lia. Qed.

(** Pointer motion approximates a time-to-position request with an
    ordered finite set of sample offsets. Requested duration includes
    the terminal sample. Application-dependent timing tolerance stays
    a dispatch parameter; the client specifies the sampling coverage
    it needs, or accepts the host's supported coverage. A scroll request
    carries a position and two signed increments. Those increments
    express input; content displacement is a separate observation. *)
Section Motion.
  Context {Position : Type}.
  Definition follows_trajectory (position_at : Tick -> Position) duration
      (samples : Intent Position) : Prop :=
    chronological samples /\
    (exists first rest, samples = first :: rest /\ at_tick first = 0) /\
    (exists last, current_view samples last /\ at_tick last = duration) /\
    Forall (fun sample => at_tick sample <= duration /\
                          value sample = position_at (at_tick sample)) samples.

  Theorem final_motion_issues_the_requested_endpoint position_at duration samples sent start slack :
    follows_trajectory position_at duration samples ->
    dispatch_valid start slack samples (issued sent) ->
    exists endpoint,
      current_view sent endpoint /\ value endpoint = position_at duration.
  Proof.
    intros [_ [_ [(last & [prefix ->] & time) correct]]] [_ scheduled].
    assert (at_end : value last = position_at duration).
    { rewrite Forall_forall in correct.
      assert (In last (prefix ++ [last])) by (apply in_or_app; now right; left).
      specialize (correct last H). destruct correct as [_ position].
      now rewrite time in position. }
    clear time correct. revert sent scheduled.
    induction prefix as [|first rest IH]; intros events scheduled; simpl in scheduled.
    - inversion scheduled as [|x y xs ys paired tail]; subst.
      inversion tail; subst. exists y. split; [now exists [] | rewrite (proj1 paired); exact at_end].
    - inversion scheduled as [|x y xs ys paired tail]; subst.
      destruct (IH _ tail) as (endpoint & [prior ->] & reaches).
      exists endpoint. split; [now exists (y :: prior) | exact reaches].
  Qed.

  Record ScrollIncrement := {
    scroll_position : Position;
    horizontal_increment : BinNums.Z;
    vertical_increment : BinNums.Z
  }.

  Definition scroll_dispatch start slack (plan : Intent ScrollIncrement) sent : Prop :=
    dispatch_valid start slack plan (issued sent).
End Motion.

(** A ready signal certifies one captured frame. A script that waits for
    it before requesting input establishes the required causal order.
    For 'act then see', ordering alone gives a later observation, not a
    guarantee that the application has already reacted. *)
Definition completed_dispatch {Event} start slack finished (plan : Intent Event)
    (result : DispatchResult Event) : Prop :=
  dispatch_valid start slack plan result /\
  match result with
  | issued sent | interrupted sent => Forall (fun e => at_tick e <= finished) sent
  end.

(** Program order supplies the causal edges. Timestamp inequalities
    allow ties at clock resolution. The report, not just the private
    sample buffer, must contain enough evidence for before/after.
    Waiting for readiness cannot force a finite window to overlap a
    later action; that depends on the actual capture and dispatch. *)
Definition spans_dispatch {Sample Evidence Image Event}
    (window : ObservationWindow Sample) (profile : ImageLayout.Profile Evidence Image)
    budget (presents : Evidence -> option (Timed Sample)) layout
    start slack finished (plan : Intent Event) sent : Prop :=
  valid_window window /\ ready_at window <= start /\
  completed_dispatch start slack finished plan (issued sent) /\
  ImageLayout.optimal profile budget (ImageLayout.layout_evidence layout) layout /\
  exists first rest last_event prefix last_frame before_evidence after_evidence,
    captured window = first :: rest ++ [last_frame] /\
    sent = prefix ++ [last_event] /\ at_tick last_event <= at_tick last_frame /\
    In before_evidence (ImageLayout.layout_evidence layout) /\ presents before_evidence = Some first /\
    In after_evidence (ImageLayout.layout_evidence layout) /\ presents after_evidence = Some last_frame.

Theorem overlapping_report_has_visible_samples_before_and_after
    {Sample Evidence Image Event} (window : ObservationWindow Sample)
    (profile : ImageLayout.Profile Evidence Image) budget presents layout
    start slack finished (plan : Intent Event) sent :
  spans_dispatch window profile budget presents layout start slack finished plan sent ->
  exists first last last_event before_evidence after_evidence,
    In first (captured window) /\ In last (captured window) /\
    current_view sent last_event /\
    In before_evidence (ImageLayout.layout_evidence layout) /\ presents before_evidence = Some first /\
    In after_evidence (ImageLayout.layout_evidence layout) /\ presents after_evidence = Some last /\
    (forall event, In event sent -> at_tick first <= at_tick event) /\
    at_tick last_event <= at_tick last.
Proof.
  intros [(head & tail & frames & _ & bounds & _) [ready [[[_ scheduled] _] [_
    (first & rest & last_event & prefix & last & eb & ea & trace & sent_end &
     after & Eb & Cb & Ea & Ca)]]]].
  assert (head = first) as -> by congruence.
  exists first, last, last_event, eb, ea. rewrite trace. repeat split; try assumption.
  - now left.
  - right. apply in_or_app. now right; left.
  - now exists prefix.
  - intros e member.
    assert (lower : forall (plans events : list (Timed Event)),
      Forall2 (scheduled_pair start slack) plans events ->
      forall event, In event events -> start <= at_tick event).
    { intros plans events matches. induction matches as [|planned event ps es Hp matched IH].
      - simpl. tauto.
      - intros x [<-|in_rest]; [|now apply IH].
        unfold scheduled_pair, due in Hp. lia. }
    specialize (lower _ _ scheduled _ member). lia.
Qed.

Theorem capture_requested_after_completion_follows_dispatch {Image Event}
    start slack finished (plan : Intent Event) sent (window : ObservationWindow Image) :
    completed_dispatch start slack finished plan (issued sent) ->
    finished <= requested_at Image window -> valid_window window ->
    forall event frame, In event sent -> In frame (captured window) ->
      at_tick event <= at_tick frame.
Proof.
  intros [_ sent_before] request (first & rest & frames & order & bounds & _) e f E F.
  rewrite Forall_forall in sent_before. specialize (sent_before e E).
  rewrite frames in F. destruct F as [<- | later]; [lia |].
  apply in_split in later. destruct later as (pre & post & split).
  assert (at_tick first <= at_tick f) as after.
  { apply (order [] first pre f post). now rewrite frames, split. }
  lia.
Qed.

(** A held control belongs to a live operation on a target. Both mouse
    buttons and keyboard keys occupy this state because a single gesture
    can depend on both. The model tracks controls the tool successfully
    held; it makes no assertion about physical keys held by the user. *)
(** The before/after use case composes a real report and a legal
    dispatch. A complete history contributes the frames that were
    actually rendered, not all samples represented by a time interval.
    A scroll mosaic contributes content, not a complete historical
    viewport. Such a viewport must be selected as a full frame. *)
Definition full_history_spans_dispatch {Target Raster LocalPoint DesktopPoint Image Event}
    (window : ObservationWindow (@Capture Target Raster LocalPoint DesktopPoint))
    (profile : ImageLayout.Profile (@FullInterval Target Raster LocalPoint DesktopPoint) Image)
    {Answer} (decide : _ -> Answer) target budget final layout
    start slack finished (plan : Intent Event) sent : Prop :=
  complete_screen_report profile decide target
    (requested_at _ window) (delivered_at window) budget (captured window) final layout /\
  spans_dispatch window profile budget (fun evidence => Some (representative evidence))
    layout start slack finished plan sent.

Definition animation_spans_dispatch {Target Raster LocalPoint DesktopPoint Image Event}
    (window : ObservationWindow (@Capture Target Raster LocalPoint DesktopPoint))
    (profile : ImageLayout.Profile (@Frame Target Raster LocalPoint DesktopPoint) Image)
    important target budget final layout start slack finished (plan : Intent Event) sent : Prop :=
  animation_report profile important target
    (requested_at _ window) (delivered_at window) budget (captured window) final layout /\
  spans_dispatch window profile budget (@Some _) layout start slack finished plan sent.

Definition mixed_spans_dispatch {Sample Target LocalPoint ContentPoint Pixel Image Event}
    (window : ObservationWindow Sample) (target_of : Sample -> Target)
    (read_pixel : Timed Sample -> LocalPoint -> option Pixel)
    (content_point : Timed Sample -> LocalPoint -> option ContentPoint)
    (profile : ImageLayout.Profile (@Tile Sample ContentPoint Pixel) Image)
    required_full target budget scroll_samples mosaic final layout
    start slack finished (plan : Intent Event) sent : Prop :=
  mixed_report target_of read_pixel content_point profile required_full target
    (requested_at _ window) (delivered_at window) budget (captured window) scroll_samples mosaic final layout /\
  spans_dispatch window profile budget
    (fun evidence => match evidence with Full frame => Some frame | Mosaic _ => None end)
    layout start slack finished plan sent.

Section Continuation.
  Context {Target Reference Invocation Button Key Position : Type}.
  Definition Control := (Button + Key)%type.
  Definition Held := Control -> Prop.
  Definition none_held : Held := fun _ => False.

  (** References distinguish live gestures from released gestures.  An
      unresolved release may be retried or reported; a closed gesture
      cannot resume. References must not be reused while an earlier
      holder could still present them. *)
  Inductive Phase := Live | CleaningUp | Closed.
  Record OperationState := {
    operation_ref : Reference;
    operation_target : Target;
    phase : Phase;
    held : Held
  }.

  Definition can_continue (state : OperationState) ref target : Prop :=
    phase state = Live /\ ref = operation_ref state /\ target = operation_target state.

  Inductive ControlEvent := Down (c : Control) | Up (c : Control) | Move (p : Position).

  Definition control_effect (event : ControlEvent) (before : Held) : Held :=
    match event with
    | Down c => fun k => k = c \/ before k
    | Up c => fun k => before k /\ k <> c
    | Move _ => before
    end.

  Definition known_held (acknowledged : list ControlEvent) (before : Held) : Held :=
    fold_left (fun controls event => control_effect event controls) acknowledged before.

  (** A delivery receipt is not a promise that the application reacted.
      An acknowledged release means the tool has evidence sufficient to
      remove the control from its logical held state. A platform failure
      with indeterminate effect needs a separate unresolved outcome. *)
  Inductive Disposition := KeepHeld | ReleaseOwned.
  Inductive CleanupResult :=
  | Settled (releases : list ControlEvent)
  | Incomplete (known_releases : list ControlEvent)
  | Indeterminate (known_releases : list ControlEvent).

  Definition same_operation (a b : OperationState) : Prop :=
    operation_ref b = operation_ref a /\ operation_target b = operation_target a.

  (** A receipt binds accepted events to this invocation's reference,
      target and dispatch. Constructing a request does not produce
      such evidence; the adapter must establish what was posted, and
      the operation owner must establish which call issued it. *)
  Record ControlDispatch := {
    dispatch_ref : Reference;
    dispatch_target : Target;
    invocation : Invocation;
    dispatch_start : Tick;
    dispatch_end : Tick;
    dispatch_slack : nat;
    dispatch_plan : Intent ControlEvent;
    dispatch_outcome : DispatchResult ControlEvent
  }.

  Definition reported_events (receipt : ControlDispatch) : list (Timed ControlEvent) :=
    match dispatch_outcome receipt with issued sent | interrupted sent => sent end.

  Definition acknowledged (receipt : ControlDispatch) : list ControlEvent :=
    map value (reported_events receipt).

  Definition receipt_valid before receipt : Prop :=
    dispatch_ref receipt = operation_ref before /\
    dispatch_target receipt = operation_target before /\
    dispatch_start receipt <= dispatch_end receipt /\
    completed_dispatch (dispatch_start receipt) (dispatch_slack receipt)
      (dispatch_end receipt) (dispatch_plan receipt) (dispatch_outcome receipt).

  (** Equal ticks are possible. The enclosing invocation supplies the
      causal order: cleanup begins after the main receipt is complete. *)
  Definition cleanup_for main cleanup : Prop :=
    invocation main = invocation cleanup /\ dispatch_end main <= dispatch_start cleanup.

  Definition resumes before ref target receipt after : Prop :=
    can_continue before ref target /\ receipt_valid before receipt /\
    (exists sent, dispatch_outcome receipt = issued sent) /\
    same_operation before after /\ phase after = Live /\
    (forall c, held after c <-> known_held (acknowledged receipt) (held before) c).

  Definition observe_operation (state : OperationState) : OperationState := state.

  Definition release_events_owned (controls : Held) (events : list ControlEvent) : Prop :=
    Forall (fun event => match event with Up c => controls c | _ => False end) events.

  Definition finishes before ref target receipt disposition cleanup result after : Prop :=
    can_continue before ref target /\ receipt_valid before receipt /\
    receipt_valid before cleanup /\ cleanup_for receipt cleanup /\ same_operation before after /\
    let controls := known_held (acknowledged receipt) (held before) in
    match disposition, result with
    | KeepHeld, Settled [] =>
        (exists sent, dispatch_outcome receipt = issued sent) /\ acknowledged cleanup = [] /\
        phase after = Live /\ forall c, held after c <-> controls c
    | ReleaseOwned, Settled releases =>
        (exists sent, dispatch_outcome cleanup = issued sent) /\
        releases = acknowledged cleanup /\ release_events_owned controls releases /\
        (forall c, controls c -> In (Up c) releases) /\ phase after = Closed /\
        (forall c, held after c <-> known_held releases controls c)
    | ReleaseOwned, Incomplete releases | ReleaseOwned, Indeterminate releases =>
        releases = acknowledged cleanup /\ release_events_owned controls releases /\
        phase after = CleaningUp /\
        (forall c, held after c <-> known_held releases controls c)
    | _, _ => False
    end.

  Theorem observation_allows_resume state ref target c :
    can_continue state ref target -> held state c ->
    can_continue (observe_operation state) ref target /\ held (observe_operation state) c.
  Proof. auto. Qed.

  Lemma release_events_remove_exactly owned events before c :
    release_events_owned owned events ->
    (known_held events before c <-> before c /\ ~ In (Up c) events).
  Proof.
    intros valid. revert before. induction valid as [|event events owns valid IH].
    - simpl. tauto.
    - intros before. destruct event as [|k|]; try contradiction.
      change (known_held events (fun x => before x /\ x <> k) c <->
              before c /\ ~ (Up k = Up c \/ In (Up c) events)).
      rewrite IH. intuition congruence.
  Qed.

  Theorem default_finish_releases_and_invalidates before ref target receipt cleanup releases after :
    finishes before ref target receipt ReleaseOwned cleanup (Settled releases) after ->
    (forall c, ~ held after c) /\ (forall ref' target', ~ can_continue after ref' target').
  Proof.
    intros [_ [_ [_ [_ [_ [_ [_ [owned [all [closed reported]]]]]]]]]]. split.
    - intros c H. apply reported in H. apply (proj1 (release_events_remove_exactly _ _ _ _ owned)) in H.
      destruct H as [H missing]. auto.
    - intros ref' target' [live _]. congruence.
  Qed.

  Theorem cleanup_cannot_precede_main_dispatch before ref target main disposition cleanup result after e :
    finishes before ref target main disposition cleanup result after ->
    In e (reported_events main) -> at_tick e <= dispatch_start cleanup.
  Proof.
    intros [_ [[_ [_ [_ [_ before_end]]]] [_ [[_ after_main] _]]]] occurs.
    destruct (dispatch_outcome main) eqn:outcome;
      unfold reported_events in occurs; rewrite outcome in occurs;
      rewrite Forall_forall in before_end; specialize (before_end _ occurs); lia.
  Qed.

  Theorem failed_cleanup_cannot_report_a_completed_release
      before ref target receipt cleanup releases after c :
    finishes before ref target receipt ReleaseOwned cleanup (Indeterminate releases) after ->
    known_held (acknowledged receipt) (held before) c -> ~ In (Up c) releases ->
    phase after = CleaningUp /\ held after c.
  Proof.
    intros [_ [_ [_ [_ [_ [_ [owned [phase_ok report]]]]]]]] H missing. split; [exact phase_ok |].
    apply report. apply (proj2 (release_events_remove_exactly _ _ _ _ owned)). auto.
  Qed.

  (** A read in the middle of a gesture needs no new down event. Once
      the agreed button was pressed, any number of motion events alone
      retain it. Application recognition and one undo remain separate
      target effects. *)
  Theorem motion_preserves_held_control (positions : list Position) before c :
    known_held (map Move positions) before c <-> before c.
  Proof. unfold known_held. induction positions; simpl; intuition. Qed.
  (** Movement needs no extra button press or release. The held
      button is established before the motion sequence and released
      by the ordinary operation finish when requested. *)
  Definition drag_motion before button position_at duration (samples : Intent Position)
      (receipt : ControlDispatch) : Prop :=
    held before (inl button) /\ receipt_valid before receipt /\
    follows_trajectory position_at duration samples /\
    dispatch_plan receipt = map (fun s => {| at_tick := at_tick s; value := Move (value s) |}) samples /\
    exists sent, dispatch_outcome receipt = issued sent.

  Theorem drag_motion_does_not_release_the_button before button position_at duration samples receipt :
    drag_motion before button position_at duration samples receipt ->
    known_held (acknowledged receipt) (held before) (inl button).
  Proof.
    intros [held_now [[_ [_ [_ [valid _]]]] [_ [plan [sent issued_ok]]]]].
    unfold acknowledged, reported_events. rewrite issued_ok.
    rewrite issued_ok, plan in valid. destruct valid as [_ sent_valid].
    assert (payloads : forall (scheduled : list (Timed Position)) actual,
      Forall2 (scheduled_pair (dispatch_start receipt) (dispatch_slack receipt))
        (map (fun s => {| at_tick := at_tick s; value := Move (value s) |}) scheduled) actual ->
      map value actual = map Move (map value scheduled)).
    { induction scheduled as [|s rest IH]; intros actual matches; simpl in *.
      - now inversion matches.
      - inversion matches as [|x y xs ys paired tail]; subst.
        simpl. now rewrite (proj1 paired), (IH _ tail). }
    rewrite (payloads _ _ sent_valid). now apply motion_preserves_held_control.
  Qed.
End Continuation.

(** Directed interaction has two independent results: work at the
    target and preservation of the human's ongoing use. The background
    requirement was confirmed by the user after trying a real app. An
    endpoint-only check misses a temporary focus or pointer change.
    A positive platform test observes both sides throughout the action,
    with comparable physical input, and checks the target result. *)
Section Background.
  Context {App Focus Point Key Text : Type}.
  Record HumanObservation := {
    foreground_app : App;
    focus_target : option Focus;
    hardware_pointer : Point;
    delivered_human_keys : list (Focus * Key);
    delivered_human_text : list (Focus * Text)
  }.

  Context {PhysicalInput Environment TargetState : Type}.
  Record BackgroundRun := {
    physical_input : PhysicalInput;
    environment : Environment;
    human_observation : Tick -> HumanObservation;
    target_before : TargetState;
    target_after : TargetState
  }.

  Definition comparable (baseline action : BackgroundRun) : Prop :=
    physical_input action = physical_input baseline /\
    environment action = environment baseline /\
    target_before action = target_before baseline.

  Definition background_success baseline action
      (expected_effect : TargetState -> TargetState -> Prop) : Prop :=
    comparable baseline action /\ expected_effect (target_before action) (target_after action) /\
    forall t, human_observation action t = human_observation baseline t.

  Theorem foreground_switch_refutes_background baseline action expected t :
    foreground_app (human_observation action t) <>
      foreground_app (human_observation baseline t) ->
    ~ background_success baseline action expected.
  Proof. intros different [_ [_ H]]. apply different. now rewrite (H t). Qed.

  Theorem moved_pointer_refutes_background baseline action expected t :
    hardware_pointer (human_observation action t) <>
      hardware_pointer (human_observation baseline t) ->
    ~ background_success baseline action expected.
  Proof. intros different [_ [_ H]]. apply different. now rewrite (H t). Qed.

  Theorem diverted_keys_refute_background baseline action expected t :
    delivered_human_keys (human_observation action t) <>
      delivered_human_keys (human_observation baseline t) ->
    ~ background_success baseline action expected.
  Proof. intros different [_ [_ H]]. apply different. now rewrite (H t). Qed.
  Theorem intra_application_focus_change_refutes_background baseline action expected t :
    focus_target (human_observation action t) <> focus_target (human_observation baseline t) ->
    ~ background_success baseline action expected.
  Proof. intros different [_ [_ H]]. apply different. now rewrite (H t). Qed.

  Theorem changed_human_text_refutes_background baseline action expected t :
    delivered_human_text (human_observation action t) <>
      delivered_human_text (human_observation baseline t) ->
    ~ background_success baseline action expected.
  Proof. intros different [_ [_ H]]. apply different. now rewrite (H t). Qed.
End Background.

(** Accessibility observations and actions are different effects. A UI
    read carries the time and the window/focus/element observations that
    the platform exposed; absent data need not mean an absent element.
    Text submission is directed at the designated input; the focus
    evidence must be valid at the submission boundary. Independent
    physical typing must not be rerouted to make a text submission work.
    Key events and submitted strings are separate capabilities whenever
    the application distinguishes their effects. An accessibility action
    is available only through the element that offers it. *)
Section UI.
  Context {App Window Element Name Bounds Value Action Text Key Operation : Type}.

  (** Some fields are unavailable, e.g. from a platform with no
      accessibility permission. An observed absence differs from an
      unavailable observation. Control references and their names are
      distinct: two elements can have the same label. *)
  Inductive Reading (A : Type) := Observed (result : A) | Unavailable.
  Arguments Observed {A} _.
  Arguments Unavailable {A}.

  Record ElementReading := {
    element_identity : Element;
    name : Reading Name;
    bounds : Reading Bounds;
    element_value : Reading Value;
    actions : Reading (list Action)
  }.

  Record UIReading := {
    app : App;
    window : Reading (option Window);
    focus : Reading (option Element);
    elements : Reading (list ElementReading);
    input_state : Operation
  }.

  Definition ui_read_valid (read : App -> Tick -> UIReading)
      (target : App) started finished (report : Timed UIReading) : Prop :=
    started <= at_tick report <= finished /\ app (value report) = target /\
    value report = read target (at_tick report).

  (** A read can be bounded and asynchronous. The observation above
      refers to what the platform acquired for the read, not an atomic
      snapshot of every property of the application. At the dispatch
      boundary, verify the designated target's identity and editable
      focus. Success of the dispatch and evidence of the target effect
      are distinct; a posted key event is not a text insertion receipt. *)
  Record Editor := {
    editor_app : App;
    editor_window : Window;
    editor_element : Element
  }.
  Variable focused : App -> Window -> Tick -> option Element.
  Variable editable : Editor -> Tick -> Prop.

  Definition focus_valid (target : Editor) t : Prop :=
    focused (editor_app target) (editor_window target) t = Some (editor_element target) /\
    editable target t.

  Definition text_submission_valid target checked
      (requested sent : Timed (Editor * Text)) : Prop :=
    value sent = value requested /\ fst (value sent) = target /\
    checked <= at_tick sent /\ focus_valid target checked /\ focus_valid target (at_tick sent).

  Theorem earlier_focus_is_insufficient target checked requested sent :
    ~ focus_valid target (at_tick sent) ->
    ~ text_submission_valid target checked requested sent.
  Proof. unfold text_submission_valid. tauto. Qed.

  (** A later observation can confirm a text effect when the target
      exposes a reliable commit event. A successful native call alone
      does not imply that this event was observed. *)
  (** The effect evidence is attributed to this submission by the
      observer, not inferred from a later matching string alone.
      Native APIs without correlation can only claim dispatch unless
      an external observation supplies the attribution. *)
  Definition observed_text_effect {Invocation}
      (attributed_commits : Invocation -> list (Timed (Editor * Text)))
      call target checked requested sent : Prop :=
    text_submission_valid target checked requested sent /\
    exists commit, attributed_commits call = [commit] /\
      at_tick sent <= at_tick commit /\ value commit = value requested.

  Theorem wrong_editor_does_not_satisfy_text_effect {Invocation}
      (commits : Invocation -> list (Timed (Editor * Text))) call target checked requested sent :
    (forall c, In c (commits call) -> value c <> value requested) ->
    ~ observed_text_effect commits call target checked requested sent.
  Proof.
    intros absent [_ (commit & only & _ & same)].
    apply (absent commit); [rewrite only; now left | exact same].
  Qed.

  (** When an app distinguishes physical keys from text commits, an
      interface whose effects are only physical key events cannot
      produce a commit. AppKit exposes key events and text commits as
      separate responder operations. Whether a particular sequence of
      keys happens to produce the desired text is app and input-method
      dependent. *)
  Inductive Effect := PhysicalKey (key : Key) | TextCommit (target : Editor) (text : Text).

  Definition key_only (effects : list Effect) : Prop :=
    Forall (fun effect => exists key, effect = PhysicalKey key) effects.

  Theorem physical_key_effects_cannot_supply_a_text_commit effects target text :
    key_only effects -> ~ In (TextCommit target text) effects.
  Proof.
    intros keys occurs. unfold key_only in keys. rewrite Forall_forall in keys.
    destruct (keys _ occurs) as [key impossible]. discriminate.
  Qed.

  Variable offers : Tick -> Element -> Action -> Prop.
  Definition action_dispatch_valid element action (sent : Timed (Element * Action)) : Prop :=
    value sent = (element, action) /\ offers (at_tick sent) element action.

  Theorem offered_action_cannot_be_dispatched_to_a_different_element element action sent other :
    action_dispatch_valid element action sent -> other <> element ->
    ~ action_dispatch_valid other action sent.
  Proof. unfold action_dispatch_valid. intros [H _] different [E _]. congruence. Qed.
End UI.

(** A script must be able to observe a result and choose the next
    request. A fixed sequence cannot in general replace this ability:
    distinct observed states can require incompatible next actions.
    This calls for data-dependent composition, without fixing the
    number of primitives or requiring that common macros be native. *)
Section Composition.
  Context {Reading Command : Type}.
  Variable correct_next : Reading -> Command -> Prop.

  Theorem fixed_next_cannot_replace_branch first second command_first command_second :
    correct_next first command_first -> correct_next second command_second ->
    (forall command, correct_next first command -> ~ correct_next second command) ->
    ~ exists command, correct_next first command /\ correct_next second command.
  Proof. intros _ _ disjoint [command [on_first on_second]]. exact (disjoint _ on_first on_second). Qed.
End Composition.

(** Preserve both obligations when substituting a platform. Every result
    must comply with the shared contract, and every request in the
    supported scope needs a positive execution. Merely failing honestly
    on every call can meet the first condition and fails the second.
    The scope includes the target's capabilities and required OS
    permissions. The shared predicates contain no native handle or API. *)
Section Portability.
  Context {Platform Request Outcome : Type}.
  Variable observed : Platform -> Request -> Outcome -> Prop.
  Variable shared_contract : Request -> Outcome -> Prop.
  Variable success : Request -> Outcome -> Prop.
  Variable required : Request -> Prop.

  Definition portable (platform : Platform) : Prop :=
    (forall request outcome, observed platform request outcome -> shared_contract request outcome) /\
    (forall request, required request ->
       exists outcome, observed platform request outcome /\ success request outcome).

  Theorem always_failing_platform_is_not_portable platform request :
    required request ->
    (forall outcome, observed platform request outcome -> ~ success request outcome) ->
    ~ portable platform.
  Proof. intros H never [_ capability]. destruct (capability request H) as (r & E & S).
    exact (never r E S). Qed.
End Portability.

(** Current evidence is scoped to the observed host. The native screenshot
    path records callback time (capture.m, capture.rs); the ScreenCaptureKit
    stream display-time documentation does not establish acquisition time
    for that path. A void Quartz post records an attempted OS dispatch;
    it cannot by itself confirm app receipt or the effect of a release.
    The AX text path reads focused element and posts as separate calls,
    so a focus change between the two defeats the stronger focus claim.
    AccessibilityRules.authorize_insertion_contract covers the sampled
    authorization check, not the focus or delivery at posting time.
    The existing desktop fixture exercises one undo for one application;
    background checks compare endpoints and do not track physical typing
    or rule out transient focus or pointer changes.

    External contracts: a transport must carry ordered script outcomes
    and attachments over MCP stdio; an embedded runtime must support
    asynchronous composition without an external language runtime.
    These are delivery constraints. Keeping dispatch/observation evidence
    supplied by platform adapters allows shared requirements to remain
    independent of platform handles. Proof obligations concern the
    semantics of those values and actual consumers, not a language split
    selected in advance. *)
(** External evidence and scope.

    The user's macOS background test supports feasibility for a real
    app. Reproduce it with simultaneous human input and continuous focus
    and cursor observation to test the trace contract; observing a
    diverted key, a pointer move or a foreground switch refutes it for
    that run. A target event report alone is insufficient.

    To test timing and rendering, compare input and capture timestamps
    against a fixture with visible transitions and known positions.
    Check a transient localized state, scroll overlap and final
    geometry, and a moving control whose relevant motion can be seen at
    the chosen sampling times. Missed transitions between samples
    remain outside the evidence that the observer can report.

    To test the gesture interpretation, a fixture records one down/up
    pair across a pause and performs one undo. A second stroke or an
    uncleared segment refutes the single-stroke claim. To test focus
    races, switch the designated field before submission and check that
    the wrong field stays unchanged. An adapter that cannot verify the
    effect must report the limitation or failure at that boundary.

    These are conditions on later implementation and acceptance.
    The current runtime is not claimed to refine these requirements. *)
