## NAME

elsh — cragen orchmynion TAIRiX

## SYNOPSIS

`elsh [-h | -?]`

## DESCRIPTION

Mae'n rhedeg cragen orchmynion ryngweithiol — dolen darllen-gwerthuso-
argraffu dros y ffrydiau safonol a etifeddwyd. Datrysir gair gorchymyn
a deipiwyd yn gyntaf yn erbyn builtins y gragen, yna storfa orchmynion
y system (`/System/Commands`), storfa raglenni'r system
(`/System/Applications`), storfa orchmynion (`<home>/Commands`) a
storfa raglenni (`<home>/Applications`) y defnyddiwr ei hun, yna
cyfeiriaduron y newidyn `PATH`; mae'r pedair storfa hyn yn rhagddodiad
sefydlog na all y defnyddiwr ei aildrefnu na'i ddisodli, felly ni all
`PATH` byth gysgodi gorchymyn system. Mae gair heb ei ddatrys yn gadael
â `127`; mae bwndel a ddatryswyd ond nad yw'n weithredadwy yn gadael â
`126`.

Y builtins:

- `cd [-L | -P [-e]] [directory]`, `cd -` — newid y cyfeiriadur gwaith:
  i `directory`, i `HOME` heb un, neu gyda `-` yn ôl i `OLDPWD`, gan ei
  argraffu. Mae `-L`, y rhagosodiad, yn cymhwyso `directory` at `PWD`
  fel y'i hysgrifennwyd tra bo `PWD` yn dal i enwi'r cyfeiriadur gwaith,
  felly mae `cd link` ac yna `cd ..` yn dod yn ôl i'r man lle mae'r
  cyswllt; mae `-P` yn dilyn y cysylltiadau yn gyntaf, ac mae `PWD` yn
  dod yn llwybr y cyfeiriadur ei hun (mae `-e` wedyn yn methu pan na
  ellir darllen y llwybr hwnnw). `/..` yw `/`, ac o dan wreiddyn alias
  (`Home:/`) nid yw `..` byth yn dringo uwchlaw'r gwreiddyn hwnnw.
- `pwd [-L | -P]` — argraffu'r cyfeiriadur gwaith: `PWD` tra bo'n dal
  i'w enwi (`-L`, y rhagosodiad), neu lwybr y cyfeiriadur ei hun (`-P`).
- `echo ...` — argraffu ei operandau.
- `export NAME=value`, `unset NAME` — golygu'r amgylchedd a
  allforiwyd.
- `jobs`, `fg`, `bg` — rheoli swyddi.
- `ulimit [-a] [-H | -S] [resource [value]]` — darllen a gosod terfynau
  adnoddau; mae `ulimit -n [value]` yn darllen neu'n gosod y terfyn ar
  ffeiliau agored.
- `umask [-S] [mode]` — dangos y mwgwd creu moddau ffeiliau (`0022`),
  neu gyda `-S` y caniatâd y mae'n ei adael (`u=rwx,g=rx,o=rx`), neu ei
  osod: `mode` wythol yw'r mwgwd ei hun, ac mae un symbolaidd (`g-w`,
  `u=rwx,g=rx,o=`) yn golygu'r caniatâd y mae'n ei adael. Mae pob
  gorchymyn a lansir wedi hynny yn creu ei ffeiliau oddi tano, ac mae
  ailgyfeiriad yn creu ei ffeil â modd `0666` llai'r mwgwd.
- `elevate <user> <program> [argument ...]` — rhedeg un gorchymyn,
  gyda'r ymresymiadau a roddwyd, wedi'i ail-ddilysu trwy oruchwyliwr
  mewngofnodi'r consol.
- `shopt [-pqsu] [-o] [name ...]` — dangos neu osod yr opsiynau ar gyfer
  patrymau enwau ffeiliau.
- `help` — rhestru'r builtins.
- `exit [code]` — gorffen y sesiwn.

Mae gair sy'n dal `*`, `?` neu `[...]` heb ddyfynodau yn batrwm enwau
ffeiliau, a rhoddir yn ei le yr enwau, wedi'u trefnu, y mae'n cyfateb
iddynt, fel yn bash: `cp *.tmp /tmp`. Rhaid i `.` ar y dechrau gael ei
gyfateb gan `.`, ac mae patrwm nad yw'n cyfateb i ddim yn aros fel y'i
hysgrifennwyd. Mae cromfachau cyrliog yn ehangu gyntaf: mae `a{b,c}` yn
rhoi `ab ac`, ac mae `{1..5}` yn cyfrif. Mae dyfynnu yn gwneud unrhyw un
o'r nodau hyn yn llythrennol, ac ni chaiff gwerth newidyn ei ddarllen
fel patrwm byth. Mae `shopt -s` yn troi opsiynau bash ymlaen: `nullglob`
(dim cyfatebiaeth, dim gair), `failglob` (mae dim cyfatebiaeth yn wall),
`dotglob`, `nocaseglob`, `globstar` (mae `**` yn cyfateb i unrhyw
ddyfnder) ac `extglob` (`?(…)`, `*(…)`, `+(…)`, `@(…)`, `!(…)`); mae
`shopt -o -s noglob` yn diffodd patrymau enwau ffeiliau.

Ar derfynell mae'r gragen yn cynnig golygydd llinell rhyngweithiol
gyda bysellau cyfarwydd bash/zsh: mae I fyny/I lawr (neu
`Ctrl-P`/`Ctrl-N`) yn pori hanes gorchmynion y sesiwn, mae `Ctrl-R` yn
ei chwilio'n gynyddrannol, mae Chwith/De/Home/End a `Ctrl-A`/`Ctrl-E`
yn symud y cyrchwr, mae `Ctrl-K`/`Ctrl-U`/`Ctrl-W`/`Ctrl-Y` yn torri ac
yn gludo, mae `Ctrl-T` yn cyfnewid dau nod, mae `Ctrl-L` yn ail-lunio
ar sgrin wedi'i chlirio, mae `Ctrl-C` yn gollwng y llinell sy'n cael ei
golygu, ac mae `Ctrl-D` ar linell wag yn gorffen y sesiwn. Mae Tab yn
cwblhau'r gair o dan y cyrchwr: enwau gorchmynion (builtins a
rhaglenni gorchymyn wedi'u gosod), llwybrau ffeiliau, a chyfeiriadau
adnoddau fel `sys:random` ar gyfer targed ailgyfeirio neu air ar ffurf
cyfeiriad. Mae gofod enwau yn cwblhau ei ddetholyddion cofrestredig
fesul segment (`state:` → `net/` → `wan/` → `link`); lle mae'r
gofrestr yn gwybod y siâp yn unig — enw rhyngwyneb, llinell ymyriad —
mae Tab yn cynnig enwau gwirioneddol y peiriant, neu ddim o gwbl os nad
yw'r sesiwn hon yn cael eu rhestru. Mae `info:`/`state:`/`stats:` yn
ymddangos fel ymresymiadau ac ar ôl `<`, sy'n darllen y gwerth
(`cat < info:mem/physical`), ond byth ar ôl `>`: ni chaiff adnodd o'r
fath ei ysgrifennu drwy ailgyfeirio. Mae mewnbwn o bibell neu o sgript
yn osgoi'r golygydd ac yn ymddwyn yr un fath gyda therfynell neu
hebddi.

Nid yw'r gragen yn cymryd operandau: nid yw rhedeg sgriptiau eto'n
rhan o'i gramadeg.

## OPTIONS

- `-h, -?` — dangos cymorth byr y gorchymyn hwn ei hun a gadael.

## EXIT STATUS

- Cod y builtin `exit`, neu `0` pan ddaw'r ffrwd fewnbwn i ben (neu
  pan ddangoswyd y cymorth byr).
- `2` — ni ddeallwyd yr alwad.

## ENVIRONMENT

- `HOME` — i ble mae `cd` yn mynd heb operand.
- `PWD`, `OLDPWD` — y cyfeiriadur gwaith fel y cyrhaeddodd `cd` ef, a'r
  un cynt; mae `cd` yn gosod y ddau. Caiff `PWD` y mae'r gragen yn
  cychwyn ag ef ei gadw tra bo'n enwi'r cyfeiriadur gwaith; fel arall
  daw'n llwybr y cyfeiriadur hwnnw ei hun.
- `PATH` — y cyfeiriaduron a chwilir ar ôl y rhagddodiad sefydlog o
  storfeydd.
- `GLOBIGNORE` — patrymau wedi'u gwahanu gan `:` y mae patrwm enwau
  ffeiliau yn gollwng eu cyfatebiaethau. Tra nad yw'r newidyn yn wag, mae
  patrwm enwau ffeiliau hefyd yn cyfateb i enwau sy'n dechrau â `.`, felly
  mae `rm *` wedyn yn dileu `.profile` hefyd.
- `LANG` — y locale a ffefrir ar gyfer y cymorth byr (tag BCP-47 fel
  `cy-GB`), wedi'i hallforio i bob gorchymyn a lansir.

## SEE ALSO

- `man`
