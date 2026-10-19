## NAME

datetime — the machine's date, time and time zone

## SYNOPSIS

`datetime`

## DESCRIPTION

Opens the **Date & Time** window: what the machine's clock reads, where that
time came from, and the controls that set it. It is reached from the desktop
clock's own menu — right-click the clock in the icon bar and choose **Set Date
& Time…** — and the desktop asks for the password of an account that may
change these settings before it opens.

The top of the window shows the time to the second, the date, and the time
zone with its abbreviation and its offset from UTC, all in the machine's own
zone.

**Date and Time** holds the clock itself.

- **Set date and time automatically** keeps the clock right from time servers
  on the network. Beneath it the window says where the clock's time came from:
  set from the network, and when; set by hand; read from the board's clock
  chip when the machine started; or not set at all. Turning the switch on or
  off takes effect at once.
- While the switch is on, the calendar and the time are shown but cannot be
  changed. Turn it off to set the clock by hand.
- Choose a day in the calendar and type or step the time, then press **Set**.
  The arrows move the chosen day by a day and a week, `Page Up` and `Page Down`
  by a month, and with `Shift` by a year; the buttons beside the month and
  the year move through them without changing the day. In the time, `Tab` or
  `:` moves from the hour to the minute to the second, and `Up` and `Down`
  step the part the cursor is in. On a twelve-hour clock the last part is
  **AM** or **PM**: click it, or press `A` or `P`. `Enter` sets the clock.
- Until you choose a day or change the time, the calendar and the time follow
  the clock as it runs. Once you have, they hold what you chose until you set
  it or close the window.

The date and time you set are read in the machine's time zone. A time the
clocks skip when they go forward is refused, and the window says when the
clocks change. A time the clocks pass twice when they go back is asked about:
choose whether you mean the one before the change or the one after it. A date
before this system was made, or more than a century after it was made, is set
only once you confirm it, because certificates would not be trusted and files
would look older, or newer, than they are.

After the clock is set the board's clock chip is set to the same time, so the
machine keeps it when it restarts. If the machine has no clock chip, the
window says the time will be lost at the next restart unless network time is
on.

**Time Zone** names the zone the machine keeps, says when its clocks next go
forward or back and by how much, and offers **Change…**, which opens a list of
every place the system knows: type part of a city's, a region's or a zone's
name to narrow it, then choose with `Enter` or a double-click. Each place shows
the time there now. A new zone takes effect at once.

The machine's zone is the one the login screen shows and the one used by
every account that has not chosen a zone of its own in **Settings**.

If the account the window runs as may not make a change, the change is
refused, an alert says why, and the window shows what is really in force.
`Escape` or **Close** closes the window.

## EXIT STATUS

Zero when the window is closed, including after a refused change. Non-zero
when the window could not be opened or its connection to the desktop was
lost; the reason is stated on the standard error stream.

## SEE ALSO

`configure`
