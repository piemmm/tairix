## NAME

tail — dangos rhan olaf ffeiliau

## SYNOPSIS

`tail [option...] [file...]`

## DESCRIPTION

Yn argraffu 10 llinell olaf pob `file` i'r allbwn safonol. Gyda mwy nag
un `file`, mae pennawd `==> file <==` yn rhagflaenu pob rhan. Heb `file`,
neu pan fo `file` yn `-`, darllenir y mewnbwn safonol.

Mae `-n` a `-c` yn newid faint a argreffir: mae cyfrif syml (neu un a
ysgrifennwyd â `-` ar y dechrau) yn argraffu'r `num` llinell neu feit
olaf; mae cyfrif a ysgrifennwyd â `+` ar y dechrau yn argraffu popeth
**o'r** llinell neu feit `num` (gan gyfrif o 1) hyd y diwedd. Gall cyfrif
gario ôl-ddodiad lluosydd: `b` (512), `kB` (1000), `K` (1024), `MB`, `M`,
`GB`, `G`, ac yn y blaen ar gyfer `T`, `P`, `E`, `Z`, `Y`, `R`, `Q` (mae
llythyren unigol yn lluosi â phwerau 1024; gyda `B` â phwerau 1000; gyda
`iB` â phwerau 1024).

Derbynnir y ffurf hanesyddol yn arg gyntaf `tail -num` / `tail +num`
(gyda llythyren derfynol `b`/`c`/`l` ddewisol), fel yn yr offeryn GNU.

Mae'r modd dilyn yn cadw pob ffeil ar agor ac yn argraffu data newydd
wrth i'r ffeil dyfu. Mae `-f` yn dilyn y ffeil agored yn ôl y
disgrifydd — mae ailenwi neu symud yn dal i ddilyn yr un ffeil. Mae `-F`
yn dilyn yr *enw*: pan gylchdroir y ffeil (ei hailenwi o'r ffordd a
rhoi ffeil newydd yn ei lle) mae'n ailagor y ffeil newydd, ac mae
`--retry` yn dal ati i roi cynnig ar enw nad yw yno eto. Mae dilyn yn
blocio nes i'r ffeil newid go iawn — nid yw byth yn troelli'r CPU. Mae
`--pid=PID` yn gorffen y dilyn unwaith y daw proses `PID` i ben, wedi'i
wirio bob `--sleep-interval` (eiliadau, rhagosodiad 1). Mae
`--max-unchanged-stats=N` yn rheoli faint o gylchoedd tawel sy'n mynd
heibio cyn i `-F` ail-wirio'r enw am gylchdro (rhagosodiad 5). Adroddir
am dorri byr (y ffeil yn crebachu) a dilynir y ffeil eto o'i dechrau
newydd.

Pan na ddangosir cynnwys blaen, ysgrifennir cofnod cynghori i'r ffrwd
gwybodaeth safonol (fd 3); nid yw byth yn newid yr allbwn na'r statws
gadael. Adroddir am ffeil na ellir ei darllen ar yr allbwn gwall ac mae'r
rhediad yn parhau â'r ffeil nesaf.

Wrth derfynell, dyfynnir pob enw a ddangosir yn yr allbwn lle byddai
angen hynny ar gragen, ac ysgrifennir unrhyw nod rheoli neu nod
anweledig arall sydd ynddo wedi'i ddianc, fel na all enw reoli nac
aildrefnu'r sgrin; mae GNU yn ei ddangos heb ei newid. Mae pibell neu
ffeil yn derbyn yr enw beit am feit.

## OPTIONS

- `-c, --bytes <num>` — argraffu `num` beit olaf pob ffeil; gyda `+` ar y
  dechrau, popeth o feit `num` ymlaen.
- `-n, --lines <num>` — argraffu `num` llinell olaf pob ffeil; gyda `+`
  ar y dechrau, popeth o linell `num` ymlaen.
- `-q, --quiet, --silent` — peidio byth ag argraffu'r penawdau
  `==> file <==`.
- `-v, --verbose` — argraffu'r penawdau `==> file <==` bob amser.
- `-z, --zero-terminated` — mae llinellau wedi'u hamffinio gan NUL yn
  lle toriad llinell.
- `-f, --follow[=descriptor]` — cadw'r ffeil ar agor ac argraffu data
  wrth iddo gael ei atodi, gan ei dilyn yn ôl y disgrifydd.
- `-F` — dilyn yn ôl yr enw (`--follow=name --retry`): ailagor y ffeil
  pan gaiff ei chylchdroi.
- `--follow=name` — dilyn yr enw yn hytrach na'r disgrifydd.
- `--retry` — dal ati i geisio agor ffeil nad oes modd ei chyrchu eto.
- `--pid <PID>` — wrth ddilyn, stopio unwaith y bydd proses `PID` yn
  marw.
- `--sleep-interval <N>` — eiliadau rhwng ail-wiriadau `--pid`/cylchdro
  (caniateir ffracsiynau; rhagosodiad 1).
- `--max-unchanged-stats <N>` — cylchoedd tawel cyn i `-F` ail-wirio'r
  enw am gylchdro (rhagosodiad 5).
- `-h, -?` — dangos cymorth byr y gorchymyn hwn.

## EXAMPLES

- `tail log.txt` — argraffu 10 llinell olaf `log.txt`.
- `tail -n 3 a b` — argraffu 3 llinell olaf `a` a `b`, pob un o dan ei
  bennawd.
- `tail -c 1K image` — argraffu 1024 beit olaf `image`.
- `tail -n +5 notes` — argraffu `notes` o'i 5ed llinell.
- `tail -f log.txt` — argraffu'r 10 llinell olaf, yna llinellau newydd
  wrth iddynt gael eu hysgrifennu.
- `tail -F /System/Logs/system` — dilyn log y system ar draws
  cylchdroadau.
- `tail -f --pid 1234 out` — dilyn `out` nes i broses 1234 orffen.

## EXIT STATUS

- `0` — argraffwyd pob ffeil (neu ysgrifennwyd y cymorth byr).
- `1` — ni ellid darllen ffeil, neu ni ellid cyflenwi'r allbwn. Mae
  llinell orchymyn na ddeallwyd hefyd yn gorffen â `1`.

## ENVIRONMENT

- `LANG` — y locale a ffefrir ar gyfer y cymorth byr (tag BCP-47 fel
  `fr-FR`).

## SEE ALSO

- `head`
- `cat`
- `wc`
- `man`
