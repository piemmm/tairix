## NAME

recorder — record the screen for the desktop

## SYNOPSIS

`recorder SESSION`

## DESCRIPTION

Records the screen into a movie while the desktop asks it to. It is not run
by hand: the desktop starts it when a recording begins, and it ends when the
recording does.

The program that starts it names the desktop session it records for as
`SESSION`.

To record, press `Meta+Shift+5` to open the capture bar, choose the whole
screen, a window or an area, and press **Record**. A capture shortcut, the
recording indicator in the notification area, or the duration chosen in
**Options** stops it.

A recording is saved as `Screen Recording YYYY-MM-DD at HH.MM.SS.webm` on the
Desktop or in `Home/Videos`, as **Options** chooses. It is a WebM movie: its
picture VP8 video at up to thirty frames a second, taken only when something
on the screen changes, and its sound Opus.

**Sound** in **Options** chooses what the recording hears: nothing, the
microphone, the desktop's own sound, or both mixed together. An
administrator's recording with sound starts at once. Any other account's asks
for an administrator's password first, and the recorder then runs as that
administrator for that one recording; cancelling records nothing, and a
notice says so.

When the disk fills, the recording ends there and is kept. When the recorder
fails, what it recorded is kept and a notice says what happened.

## EXIT STATUS

Zero when the recording finished, including one the disk filled. Non-zero
when it could not begin or ended any other way; the reason is stated on the
standard error stream.

## SEE ALSO

`desktop`
