## NAME

blendcheck — cymharu cymysgu'r peiriant arddangos â chymysgu'r bwrdd gwaith

## SYNOPSIS

`blendcheck`

## DESCRIPTION

Mae'n cymryd y sgrin drosodd ac yn dangos graddiant llwyd ar draws ei
lled, gyda thri chwarel o wydr tywyll drosto, un uwchben y llall:

1. wedi'i gymysgu gan y bwrdd gwaith, a'i wasgaru (dithering), fel y
   mae'r bwrdd gwaith yn lluniadu ffenestri tryleu;
2. wedi'i gymysgu gan y bwrdd gwaith heb y gwasgaru, sy'n dangos y
   grisiau y mae bandio'n eu gadael mewn graddiant llyfn;
3. wedi'i gymysgu gan galedwedd yr arddangosfa ei hun.

Os yw'r cwarel isaf yn edrych fel yr un uchaf, mae caledwedd yr
arddangosfa'n cymysgu tryleuedd heb fandio. Os yw'n dangos grisiau'r
un canol, nid yw'n gwneud hynny. Mae'r gorchymyn hefyd yn argraffu a
yw gyrrwr yr arddangosfa'n datgan bod ei chymysgu'n rhydd o fandio.

Pwyswch unrhyw fysell neu fotwm pwyntydd i roi'r sgrin yn ôl.

Mae angen yr arddangosfa ar y gorchymyn iddo'i hun, felly rhedwch ef o
sesiwn destun yn hytrach nag o derfynell ar y bwrdd gwaith. Nid oes gan
arddangosfa heb haenau caledwedd unrhyw gymysgu ei hun i'w gymharu, ac
mae'r gorchymyn yn dweud hynny.

## OPTIONS

- `-h, -?` — dangos cymorth byr y gorchymyn hwn ei hun.

## EXAMPLES

- `blendcheck` — dangos y gymhariaeth nes pwyso bysell.

## EXIT STATUS

- `0` — dangoswyd a chaewyd y gymhariaeth.
- `1` — roedd yr arddangosfa'n cael ei dal gan sesiwn, nid oes ganddi
  haenau caledwedd, gwrthododd hi'r patrwm, neu gollwyd y sedd tra
  oedd yn cael ei ddangos. Mae llinell orchymyn na ddeallwyd hefyd yn
  gorffen â `1`.

## ENVIRONMENT

- `LANG` — y locale a ffefrir ar gyfer y cymorth byr (tag BCP-47 fel
  `fr-FR`).

## SEE ALSO

- `sysinfo`
- `man`
