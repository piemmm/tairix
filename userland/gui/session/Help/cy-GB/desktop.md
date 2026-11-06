## NAME

desktop — cychwyn y sesiwn bwrdd gwaith graffigol

## SYNOPSIS

`desktop`

## DESCRIPTION

Yn cychwyn y sesiwn bwrdd gwaith graffigol ar sedd y peiriant hwn:
mae'r gorchymyn yn caffael prydles arddangos a mewnbwn unigryw'r sedd,
yn cysylltu â'r gwasanaeth arddangos, ac yn rhedeg y bwrdd gwaith
cyfansoddol — y rheolwr ffenestri a'r bar tasgau — nes i'r sesiwn ddod
i ben. Mae'r gorchymyn yn dychwelyd pan ddaw'r sesiwn bwrdd gwaith i
ben.

Mae'r un bwrdd gwaith yn cychwyn yn awtomatig ar ôl dilysu: mewngofnodi
graffigol (`os.loginType`) yw'r rhagosodiad ar beiriant sy'n gallu ei
redeg. Mae'r gorchymyn hwn yn ei gychwyn ar alw o gragen destun.

Pan nad oes gwasanaeth arddangos yn rhedeg, neu pan fo sesiwn arall
eisoes yn dal y sedd, mae'r gorchymyn yn methu gan ysgrifennu ei reswm
i'r allbwn gwall safonol — nid yw byth yn disodli sesiwn sy'n rhedeg.

Tra bo'r bwrdd gwaith yn rhedeg, mae `Meta+Shift+3` yn cadw llun o'r
sgrin a `Meta+Shift+4` lun o ardal rydych yn ei llusgo — neu, ar ôl
`Space`, o ffenestr rydych yn clicio arni. Mae `Meta+Shift+5` yn agor y
bar cipio, sydd hefyd yn recordio'r sgrin. Mae ychwanegu `Ctrl` yn rhoi'r
llun ar y clipfwrdd, ac mae llwybr byr cipio a wasgir yn ystod recordiad
yn ei atal. Mae recordiad gyda sain yn gofyn am gyfrinair gweinyddwr pan
na chaiff eich cyfrif recordio sain ei hun.

## OPTIONS

- `-h, -?` — dangos cymorth byr y gorchymyn hwn.

## EXAMPLES

- `desktop` — cychwyn y sesiwn bwrdd gwaith.

## EXIT STATUS

- `0` — cafodd y cymorth byr ei ddangos.
- `1` — ni ddeallwyd y llinell orchymyn.
- unrhyw god arall nad yw'n sero — ni allai'r sesiwn gychwyn (dim sedd,
  dim gwasanaeth arddangos) neu daeth i ben (collwyd prydles y sedd);
  ysgrifennir y rheswm i'r allbwn gwall safonol.

## ENVIRONMENT

- `LANG` — y locale a ffefrir ar gyfer y cymorth byr (tag BCP-47 megis
  `fr-FR`).

## SEE ALSO

- `configure`
- `man`
