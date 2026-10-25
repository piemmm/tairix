## NAME

chmod — newid didau modd ffeil

## SYNOPSIS

`chmod [-cfRv] [--] MODE file...`

## DESCRIPTION

Yn newid didau caniatâd pob operand ffeil i `MODE`, yn eu trefn. Mae
`MODE` naill ai'n werth wythol absoliwt (`644`, `0755`, …) sy'n
disodli'r didau caniatâd yn llwyr, neu'n rhestr o gymalau symbolaidd
wedi'u gwahanu gan atalnodau, `[ugoa]*([-+=]([rwxXst]*|[ugo]))+` neu
`[-+=][0-7]+` (`g+w`, `o-rx`, `a=rx`, `u+s`, `g=u`, `=644`), sy'n
trawsffurfio didau presennol y ffeil. Dim ond i gyfeiriadur, neu i
ffeil sydd eisoes yn cario did gweithredu, y mae'r `X` symbolaidd yn
rhoi gweithredu; mae `g=u` yn rhoi didau'r perchennog i'r grŵp, mae `s`
yn gosod didau set-ID y perchennog a'r grŵp, ac mae `t` yn gosod y did
gludiog. Mae cymal nad yw'n enwi neb (`+x`, `=r`) yn newid pob maes ac
eithrio'r didau sydd yn yr umask.

Ar gyfeiriadur, mae'r didau set-user-ID a set-group-ID yn aros fel y
maent oni bai bod y modd yn eu henwi: gydag `s` (`g-s`), gyda digidau
wythol ar ôl gweithredydd (`=755`), neu gyda gwerth wythol o bum digid
neu fwy (`00755`). Did set-group-ID cyfeiriadur yw'r hyn sy'n rhoi ei
grŵp i'w gofnodion newydd, felly mae `chmod 755` yn ei gadw.

Dim ond perchennog ffeil all newid ei modd; mae'r cnewyllyn yn
gwrthod unrhyw un arall, ac nid yw dal capability yn rhoi unrhyw
oruchafiaeth. Mae cyswllt symbolaidd a enwir fel operand yn newid y
ffeil y mae'n ei henwi. Gyda `-R` caiff operand cyfeiriadur ei newid ac
yna caiff ei gynnwys ei newid yn ailadroddus; gadewir llonydd i gyswllt
symbolaidd a geir y tu mewn i'r goeden, ac i'r hyn y mae'n ei enwi
hefyd. Adroddir am ffeil na ellir ei chyrraedd na'i newid, a newidir y
gweddill o hyd. Mae `--` yn gorffen dosrannu opsiynau: mae pob dadl
ddiweddarach yn operand. Ar gyfer modd sy'n dechrau gyda `-`,
ysgrifennwch ef heb y llinell doriad (`a-w`) neu gorffennwch yr
opsiynau'n gyntaf (`chmod -- -w file`).

## OPTIONS

- `-R, --recursive` — newid ffeiliau a chyfeiriaduron yn
  ailadroddus.
- `-c, --changes` — adrodd dim ond y ffeiliau y newidiodd eu modd
  mewn gwirionedd.
- `-v, --verbose` — adrodd pob ffeil a broseswyd, gan gynnwys un y
  gwrthodwyd ei newid neu na ellid ei chyrraedd.
- `-f, --silent, --quiet` — atal y rhan fwyaf o negeseuon gwall;
  mae'r rhediad yn dal i fethu ac mae'r statws gadael yn ei adrodd.
- `--preserve-root` — gydag `-R`, gwrthod newid `/`, sut bynnag y'i
  cyrhaeddir.
- `--no-preserve-root` — trin `/` fel unrhyw gyfeiriadur arall (y
  rhagosodiad).
- `-h, -?, --help` — dangos cymorth byr y gorchymyn hwn.

## EXAMPLES

- `chmod 644 notes.txt` — darllen/ysgrifennu i'r perchennog,
  darllen yn unig i bawb arall.
- `chmod g+w shared.txt` — ychwanegu ysgrifennu grŵp at y didau
  presennol.
- `chmod -R a=rx Docs` — gwneud y goeden `Docs` yn ddarllenadwy ac
  yn dramwyadwy i bawb.
- `chmod +x build.sh` — gwneud sgript yn weithredadwy i bawb y mae'r
  umask yn eu caniatáu.

## EXIT STATUS

- `0` — llwyddodd pob newid modd.
- `1` — ni ellid cyrraedd na newid ffeil, neu methodd yr allbwn; caiff
  pob rheswm ei argraffu ar yr allbwn gwall (wedi'i atal o dan `-f`).
  Mae llinell orchymyn na ddeallwyd, neu nad oedd ei hoperand modd yn
  wythol nac yn symbolaidd, hefyd yn gorffen â `1`.

## ENVIRONMENT

- `LANG` — y locale a ffefrir ar gyfer y cymorth byr (tag BCP-47
  megis `cy-GB`).

## SEE ALSO

- `ls`
- `mkdir`
- `rm`
