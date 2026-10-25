## NAME

chown — newid perchennog a grŵp ffeil

## SYNOPSIS

`chown [OPTION]... [OWNER][:[GROUP]] file...`

`chown [OPTION]... --reference=RFILE file...`

## DESCRIPTION

Mae'n newid y defnyddiwr a'r grŵp sy'n berchen ar bob operand ffeil, yn
eu trefn. Mae `OWNER` yn newid y defnyddiwr ac yn cadw'r grŵp, mae
`OWNER:GROUP` yn newid y ddau, ac mae `:GROUP` yn newid y grŵp yn unig.
Mae `OWNER:` hefyd yn rhoi grŵp mewngofnodi'r perchennog i'r ffeil. Nid
yw operand gwag, na `:` ar ei ben ei hun, yn newid y naill na'r llall.
Mae'r hen sillafiad `OWNER.GROUP` yn gweithio gyda rhybudd, oni bai bod
yr operand cyfan yn enwi cyfrif. Gyda `--reference`, mae pob ffeil yn
cymryd perchennog a grŵp yr hyn y mae `RFILE` yn ei enwi.

Enwau cyfrifon a grwpiau, neu rifau, yw `OWNER` a `GROUP`: rhoddir
cynnig ar enw yn gyntaf, a dim ond pan nad oes gan ddim yr enw hwnnw y
cymerir rhif. Mae `+` ar y dechrau (`+1000`) bob amser yn golygu rhif.

Mae rhoi ffeil i gyfrif arall, neu i grŵp nad ydych yn aelod ohono, yn
gofyn am y gallu `CAP_FS_CHOWN`; hebddo gallwch newid grŵp ffeil yr
ydych yn berchen arni i un o'ch grwpiau eich hun. Mae newid yn clirio
did set-user-ID ffeil, a'i did set-group-ID pan gaiff ei grŵp ei rhedeg;
mae cyfeiriadur yn cadw'r ddau.

Mae cyswllt symbolaidd a enwir fel operand yn newid y ffeil y mae'n ei
henwi, neu gydag `-h` y cyswllt ei hun. Gydag `-R` caiff operand
cyfeiriadur a phopeth ynddo eu newid, pob cyfeiriadur ar ôl ei gynnwys.
Caiff cyswllt symbolaidd a geir y tu mewn i'r goeden ei newid ei hun, ac
ni chyffyrddir byth â'r hyn y mae'n ei enwi; gyda `--dereference`
gadewir llonydd i'r cyswllt hefyd. Gydag `-R` caiff cyswllt symbolaidd a
enwir fel operand ei newid ei hun, ac ni chaiff ei ddilyn: mae
`--dereference` yn newid yr hyn y mae'n ei enwi yn lle hynny, ac mae
`-H` yn tramwyo cyfeiriadur y mae'n ei enwi. Adroddir am ffeil na ellir
ei chyrraedd na'i newid, a newidir y gweddill o hyd. Mae adroddiad yn
enwi pob perchennog a grŵp wrth ei enw, neu wrth ei rif pan nad oes gan
unrhyw gyfrif na grŵp y rhif hwnnw. Mae `--` yn gorffen dosrannu
opsiynau: mae pob dadl ddiweddarach yn operand.

## OPTIONS

- `-c, --changes` — adrodd dim ond y ffeiliau y newidiodd eu perchennog
  neu eu grŵp mewn gwirionedd.
- `-f, --silent, --quiet` — atal y rhan fwyaf o negeseuon gwall; mae'r
  rhediad yn dal i fethu ac mae'r statws gadael yn ei adrodd.
- `-v, --verbose` — adrodd pob ffeil a broseswyd, gan gynnwys un y
  gwrthodwyd ei newid neu na ellid ei chyrraedd.
- `--dereference` — newid yr hyn y mae cyswllt symbolaidd yn ei enwi yn
  hytrach na'r cyswllt; heb `-R` dyma'r rhagosodiad.
- `-h, --no-dereference` — newid cyswllt symbolaidd ei hun yn hytrach
  na'r hyn y mae'n ei enwi.
- `--preserve-root` — gydag `-R`, gwrthod newid `/`, sut bynnag y'i
  cyrhaeddir.
- `--no-preserve-root` — trin `/` fel unrhyw gyfeiriadur arall (y
  rhagosodiad).
- `--from=CURRENT_OWNER:CURRENT_GROUP` — newid ffeil dim ond os mai'r
  rhain yw ei pherchennog a'i grŵp presennol; gadewch y naill neu'r
  llall allan i dderbyn unrhyw un.
- `--reference=RFILE` — rhoi perchennog a grŵp `RFILE` i bob ffeil yn
  lle eu henwi.
- `-R, --recursive` — newid ffeiliau a chyfeiriaduron yn ailadroddus.
- `-H` — gydag `-R`, tramwyo'r cyfeiriadur y mae cyswllt symbolaidd a
  enwir fel operand yn arwain ato.
- `-L` — gydag `-R`, dilyn pob cyswllt symbolaidd i gyfeiriadur; fe'i
  gwrthodir, gan nad yw newid ailadroddus byth yn dilyn cyswllt y tu
  mewn i'r goeden.
- `-P` — gydag `-R`, peidio â dilyn unrhyw gyswllt symbolaidd (y
  rhagosodiad). Yr olaf a roddir o `-H`, `-L` a `-P` sy'n ennill.
- `-?, --help` — dangos cymorth byr y gorchymyn hwn ei hun.
- `--version` — argraffu enw a fersiwn yr offeryn.

## EXAMPLES

- `chown alice notes.txt` — rhoi `notes.txt` i `alice`, gan gadw ei
  grŵp.
- `chown alice:staff notes.txt` — ei rhoi i `alice` a'r grŵp `staff`.
- `chown :staff shared.txt` — newid y grŵp yn unig.
- `chown -R alice Projects` — rhoi coeden `Projects` i `alice`.
- `chown -R --from=bob alice Shared` — rhoi i `alice` dim ond yr hyn
  sy'n eiddo i `bob` yn `Shared`.
- `chown --reference=template.txt notes.txt` — rhoi perchennog a grŵp
  `template.txt` i `notes.txt`.

## EXIT STATUS

- `0` — llwyddodd pob newid.
- `1` — ni ellid cyrraedd na newid ffeil, neu methodd yr allbwn; caiff
  pob rheswm ei argraffu ar yr allbwn gwall (wedi'i atal o dan `-f`).
  Mae llinell orchymyn na ddeallwyd, neu berchennog, grŵp neu ffeil
  gyfeirio na ellid eu darllen, hefyd yn gorffen â `1` ac yn newid dim.

## ENVIRONMENT

- `LANG` — y locale a ffefrir ar gyfer y cymorth byr (tag BCP-47 fel
  `cy-GB`).

## SEE ALSO

- `chmod`
- `ls`
- `stat`
