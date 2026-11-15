# Desktop helpers

An automation can start from an app name when it has no window or process
identifier. Menu discovery and text selection similarly need information
outside the window tree or operations on a specific AX attribute. The
`desktop` namespace distinguishes these helpers from the window and input
functions. Keeping them in `run` lets scripts launch, discover, observe and
act using the same scheduling and error handling.

App discovery and launch use the system workspace and application bundles.
App names can be ambiguous, so a caller can resolve the choice using a
bundle identifier. Launch requests avoid activation so the caller can wait
for a background window without first taking keyboard focus.

Menu bars share AX traversal and references with window reads. This lets a
script act on a menu item through the same action interface as any other
element. Text selection uses a range on the retained element because
simulating cursor movement depends on the app's current selection.

Rich paste needs the system clipboard to give the receiving app a choice of
representations. The operation uses the focused editor check shared with
plain text insertion. It serializes clipboard use across Selvedge processes
and restores clipboard items only while its change count still matches.
Quartz posts events asynchronously. The paste receipt describes posting
and clipboard restoration, and the caller can verify the edit by reading
the target.
