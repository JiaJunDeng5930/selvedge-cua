# Window discovery and screenshot detail

A drawing session spent time activating Chrome and retrying work when an AX
tree had no usable window ID. Another tool may also need to choose between
several windows of a background app. AppKit supplies process identity,
Core Graphics supplies window identity and visibility, and AX supplies the
control tree. Keeping discovery read-only lets the caller choose a window
without changing the user's focus. Geometry and a unique title match relate
the AX and Core Graphics observations. If these observations are ambiguous,
the caller can use the compositor ID for an exact screenshot and report the
ambiguity in the AX read. Input rechecks the live destination because a
successful read cannot freeze native ownership or geometry.

An overview is useful for layout and broad tones. The caller chooses its
resolution before observation, so evidence extraction and packing use the
resolution the caller asked to inspect. The frame retains its original
desktop rectangle and capture time. This gives the caller one coordinate
system for both an overview and detailed follow-up. Keeping a single
resolution option on the observation also avoids changing the existing
image-count budget or introducing a separate screenshot operation.
