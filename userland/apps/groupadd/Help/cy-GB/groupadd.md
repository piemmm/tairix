## NAME

groupadd — creu grŵp

## SYNOPSIS

`groupadd [-g GID] [--] NAME`

## DESCRIPTION

Mae'n ychwanegu un grŵp at gofrestr y grwpiau. Rhaid i enw'r grŵp
gyfateb i `[a-z_][a-z0-9_-]*` a gwerth degol yw'r id. Gweithred
weinyddol yw creu grŵp: mae'r gofrestr yn gwrthod galwr heb allu
gweinyddu defnyddwyr.

Pan hepgorir `-g`, dyrennir id y grŵp yn awtomatig: un uwchlaw'r id
uchaf sy'n bodoli o 1000 neu fwy, neu 1000 pan nad oes gan unrhyw grŵp
un. Mae idau islaw 1000 yn eiddo i'r system ac ni chânt byth eu
dyrannu, ac unwaith na fydd id ar ôl uwchlaw'r uchaf, ni chaiff y grŵp
ei greu. Gwrthodir id y gofynnwyd amdano sydd eisoes wedi'i gymryd; y
gofrestr yw'r awdurdod ar wrthdrawiadau.

Mae `--` yn gorffen dosrannu opsiynau: mae pob ymresymiad diweddarach
yn operand.

## OPTIONS

- `-g, --gid GID` — id rhifol y grŵp; fe'i dyrennir yn awtomatig pan
  hepgorir (un uwchlaw'r id uchaf sy'n bodoli o 1000 neu fwy, neu 1000
  fel arall).
- `-h, -?, --help` — dangos cymorth byr y gorchymyn hwn ei hun.

## EXAMPLES

- `groupadd staff` — creu `staff` gydag id a ddyrannwyd yn awtomatig.
- `groupadd -g 100 staff` — creu `staff` gyda'r id `100`.

## EXIT STATUS

- `0` — crëwyd y grŵp.
- `1` — gwrthododd neu fethodd y gofrestr y creu (er enghraifft gallu
  coll neu id dyblyg); argraffir y rheswm ar y gwall safonol. Mae
  llinell orchymyn na ddeallwyd hefyd yn gorffen â `1`.

## ENVIRONMENT

- `LANG` — y locale a ffefrir ar gyfer y cymorth byr (tag BCP-47 fel
  `cy-GB`).

## SEE ALSO

- `useradd`
- `users`
