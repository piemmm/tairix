## NAME

clear — clirio sgrin y derfynell

## SYNOPSIS

`clear [-x]`

## DESCRIPTION

Mae'n ysgrifennu'r dilyniant sy'n symud y cyrchwr i'r gornel chwith
uchaf ac yn dileu'r arddangosfa gyfan, gan adael sgrin wag. Y derfynell
a enwir yn `TERM` sy'n penderfynu pa ddilyniant a ysgrifennir; mae
terfynell na all glirio (mae `TERM` anhysbys yn dirywio i'r sylfaen
«dumb») yn gwneud i'r gorchymyn fethu yn hytrach nag argraffu beitiau y
byddai'r derfynell yn eu dangos fel sbwriel.

Lle gall y derfynell, caiff y llinellau y mae'n eu cadw uwchben y
sgrin — yr ôl-sgrolio — eu dileu hefyd, fel na ellir sgrolio'n ôl
at ddim a oedd ar y sgrin.

## OPTIONS

- `-x` — cadw'r ôl-sgrolio: dileu'r sgrin yn unig.
- `-h, -?` — dangos cymorth byr y gorchymyn hwn ei hun.

## EXAMPLES

- `clear` — clirio'r sgrin.

## EXIT STATUS

- `0` — ysgrifennwyd y dilyniant clirio.
- `1` — ni all y derfynell glirio, neu ni ellid cyflwyno'r allbwn. Mae
  llinell orchymyn na ddeallwyd hefyd yn gorffen â `1`.

## ENVIRONMENT

- `TERM` — y derfynell yr ysgrifennir ei dilyniant clirio.
- `LANG` — y locale a ffefrir ar gyfer y cymorth byr (tag BCP-47 fel
  `cy-GB`).

## SEE ALSO

- `reset`
- `man`
