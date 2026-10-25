## NAME

mkdir — creu cyfeiriaduron

## SYNOPSIS

`mkdir [-pv] [-m MODE] [--] directory...`

## DESCRIPTION

Mae'n creu pob operand cyfeiriadur, yn eu trefn. Heb `-p` rhaid i
riant pob operand fodoli eisoes ac ni chaiff yr operand ei hun fodoli;
adroddir am operand na ellir ei greu a chrëir yr un nesaf o hyd.

Gydag `-p` crëir pob hynafiad coll yn gyntaf, y mwyaf allanol yn
gyntaf, ac nid yw operand (neu hynafiad) sydd eisoes yn bodoli fel
cyfeiriadur yn wall. Mae hynafiad sy'n bodoli fel ffeil yn dal i
fethu: ni ddisodlir dim byth yn dawel.

Mae cyfeiriadur newydd yn cael y modd `a=rwx` llai'r umask. Gydag `-m`
mae pob operand cyfeiriadur a grëir yn cael `MODE` yn union yn lle
hynny, beth bynnag fo'r umask: gwerth wythol neu gymalau symbolaidd, fel
y mae `chmod` yn eu darllen, gan ddechrau o `a=rwx`. Mae hynafiad coll y
mae `-p` yn ei greu yn cadw caniatâd ysgrifennu a chwilio ei berchennog
beth bynnag a ddywed yr umask, fel y gellir creu'r cyfeiriadur oddi tano
bob amser. Mae cyfeiriadur a grëir y tu mewn i gyfeiriadur set-group-ID
yn cadw'r did y mae'n ei etifeddu oni bai bod `MODE` yn ei enwi. Mae
operand sydd eisoes yn bodoli yn cadw ei fodd. Mae `--` yn gorffen
dosrannu opsiynau: mae pob ymresymiad diweddarach yn llwybr.

## OPTIONS

- `-m, --mode=MODE` — rhoi'r modd `MODE` i bob operand cyfeiriadur a
  grëir, yn wythol neu'n symbolaidd fel yn `chmod`, yn hytrach nag
  `a=rwx` llai'r umask.
- `-p, --parents` — creu cyfeiriaduron rhiant coll; nid yw operand
  sydd eisoes yn gyfeiriadur yn wall.
- `-v, --verbose` — adrodd am bob cyfeiriadur a grëwyd fel
  `mkdir: created directory 'dir'`.
- `-h, -?` — dangos cymorth byr y gorchymyn hwn ei hun (hefyd
  `--help`).

## EXAMPLES

- `mkdir Notes` — creu un cyfeiriadur yn y cyfeiriadur cyfredol.
- `mkdir -p Projects/os/build` — creu'r gadwyn gyfan, gan hepgor y
  rhannau sydd eisoes yn bodoli.
- `mkdir -pv Home:/tools/bin` — creu o dan wreiddyn alias, gan adrodd
  am bob cyfeiriadur newydd.
- `mkdir -m 700 Private` — creu cyfeiriadur na all neb ond ei
  berchennog ei ddefnyddio.

## EXIT STATUS

- `0` — crëwyd pob cyfeiriadur (neu, o dan `-p`, roedd eisoes yn
  bodoli).
- `1` — methiant system ffeiliau neu allbwn, neu fodd na ellid ei roi;
  argraffir y rheswm ar y gwall safonol. Mae llinell orchymyn na
  ddeallwyd, neu `MODE` nad yw'n fodd, hefyd yn gorffen â `1`.

## ENVIRONMENT

- `LANG` — y locale a ffefrir ar gyfer y cymorth byr (tag BCP-47 fel
  `cy-GB`).

## SEE ALSO

chmod, rmdir, rm, ls
