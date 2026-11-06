## NAME

fstree — y rheolwr ffeiliau coeden sgrin lawn

## SYNOPSIS

`fstree [directory]`

## DESCRIPTION

Yn pori'r system ffeiliau mewn sesiwn sgrin lawn a yrrir gan y
bysellfwrdd, wedi'i modelu ar XTree Gold: pennyn ystadegau disg ar y
brig, ffenestr coeden gyfeiriaduron oddi tano uwchben ffenestr ffeiliau
sy'n rhestru cofnodion y cyfeiriadur a ddewiswyd gyda'u meintiau a'u
hamserau addasu. Mae'r sesiwn yn dechrau yn `directory` (y golwg gwraidd
`/` yn ddiofyn).

Y ffenestr coeden yw'r brif un: mae'r cyrchwr yn symud drwyddi tra bo'r
ffenestr ffeiliau bob amser yn dangos y cyfeiriadur a ddewiswyd. Mae
`Enter` (neu `Tab`) yn mynd i mewn i'r ffenestr ffeiliau i weithredu ar
ffeiliau, ac mae `Esc` (neu `Tab`) yn dychwelyd i'r goeden — mae'r
llywio bob amser yn dechrau yn y goeden.

Darllenir y goeden yn ddiog: dim ond pan gaiff ei ddangos neu ei ehangu
am y tro cyntaf y cyrchir cynnwys cyfeiriadur, felly dim ond y
cyfeiriaduron a agorwyd mewn gwirionedd yw cost pori cyfrol enfawr. Caiff
cyfeiriadur na chaiff y galwr ei restru ei wrthod yn y fan a'r lle —
ymddengys y gwall ar y llinell negeseuon a chedwir y golwg blaenorol; ni
chaiff dim ei ffugio.

Bysellau:

- `Up`/`Down` neu `k`/`j` — symud cyrchwr y ffenestr weithredol; mae
  `PageUp`/`PageDown` yn symud fesul sgrinaid, ac mae `Home`/`End` yn
  neidio i'r rhes gyntaf/olaf. Mae symud cyrchwr y goeden yn rhestru'r
  cyfeiriadur newydd ei ddewis yn y ffenestr ffeiliau.
- `Right`/`l` — yn y goeden, ehangu'r gangen a ddewiswyd (gan ei
  darllen yn ddiog); yn y ffenestr ffeiliau, disgyn i'r cyfeiriadur a
  ddewiswyd.
- `Left`/`h` — yn y goeden, cau'r gangen a ddewiswyd, neu — pan fo
  eisoes ar gau neu heb is-gyfeiriaduron — symud y cyrchwr i fyny i'r
  cyfeiriadur rhiant; yn y ffenestr ffeiliau, dychwelyd i'r goeden.
- `+`/`-` — ehangu / cau'r gangen a ddewiswyd yn y goeden.
- `Enter` — yn y goeden, mynd i mewn i'r ffenestr ffeiliau; yn y
  ffenestr ffeiliau, disgyn i'r cyfeiriadur a ddewiswyd (mae'r ddwy
  ffenestr yn dilyn), neu agor y ffeil a ddewiswyd mewn gwyliwr sgrin
  lawn: y gwyliwr dadosodiad ar gyfer ffeil weithredadwy a adnabyddir, y
  tudalennwr testun pan fo beitiau cyntaf y ffeil yn darllen fel testun,
  a'r dymp hecs fel arall (disgrifir y gwylwyr isod).
- `Esc` — canslo cyfrif defnydd disg sy'n rhedeg (gan gadw ei
  ffigurau); fel arall dychwelyd o'r ffenestr ffeiliau i'r ffenestr
  coeden.
- `o` — agor y ffeil a ddewiswyd yn y gwyliwr o'ch dewis: `t` testun,
  `x` hecs, neu `d` dadosodiad.
- `Tab` — newid rhwng y ffenestr coeden a'r ffenestr ffeiliau.
- `s` — agor y ddewislen didoli: `n` enw, `e` estyniad, `s` maint,
  `m` amser addasu, `r` gwrthdroi'r cyfeiriad, `Esc` yn canslo. Caiff
  cyfeiriaduron eu grwpio bob amser cyn ffeiliau.
- `c` — copïo'r cofnod a ddewiswyd: mae llinell fewnbwn yn gofyn am y
  gyrchfan. Mae cyrchfan gymharol yn glanio yn y cyfeiriadur a
  restrwyd; mae cyrchfan sy'n gyfeiriadur presennol yn derbyn y copi
  y tu mewn iddo o dan enw'r ffynhonnell. Mae `Tab` yn cwblhau'r llwybr
  a deipiwyd — caiff cydweddiad unigryw ei lenwi (gyda chyfeiriadur yn
  aros ar agor gyda'i `/`), ac mae sawl cydweddiad yn ymestyn at eu
  bôn cyffredin neu'n cael eu rhestru. Caiff cyfeiriadur ei gopïo
  gyda phopeth oddi tano. Gwrthodir copïo cofnod ar ei ben ei hun neu
  gyfeiriadur i mewn i'w is-goeden ei hun cyn ysgrifennu dim.
- `m` — symud y cofnod a ddewiswyd, gyda'r un cwestiwn cyrchfan. O
  fewn yr un gyfrol mae'r symud yn ailenwi atomig; ar draws cyfrolau
  caiff y cofnod ei gopïo ac yna dilëir y ffynhonnell.
- `r` — ailenwi'r cofnod a ddewiswyd yn ei le: mae'r llinell fewnbwn
  wedi'i rhag-lenwi â'r enw presennol.
- `d` — dileu'r cofnod a ddewiswyd ar ôl cadarnhad; dim ond `y` sy'n
  bwrw ymlaen. Mae dileu cyfeiriadur yn tynnu popeth oddi tano, ac
  mae'r cadarnhad yn dweud hynny. Gellir diffodd y cadarnhad yn y
  ddewislen gosodiadau (`S`).
- `M` — creu cyfeiriadur yn y cyfeiriadur a restrwyd; gofynnir am ei
  enw.
- `a` — agor priodoleddau'r cofnod a ddewiswyd: ei ddidau caniatâd a'i
  briodoleddau estynedig (parau `namespace.key = value`, e.e.
  `user.comment`). Yn y golwg, mae'r saethau'n symud dros y
  priodoleddau, mae `m` yn golygu'r modd (llinell wythol wedi'i
  rhag-lenwi â'r didau presennol; mae Enter yn cymhwyso — dim ond
  perchennog y cofnod all ei newid, mae'r cnewyllyn yn gwrthod pawb
  arall), mae `n` yn ychwanegu priodoledd fel llinell `key=value`, mae
  Enter yn golygu'r un a ddewiswyd (wedi'i rhag-lenwi; gyda gwerth
  deuaidd dim ond yr allwedd a rag-lenwir), mae `d` yn ei thynnu, ac mae
  Esc yn gadael. Y cnewyllyn sy'n penderfynu pob newid — mae darllen ac
  ysgrifennu'n dilyn caniatâd y cofnod ei hun, mae'r gofodau enwau
  `system.`/`trusted.` wedi'u cadw, a dangosir gwrthodiad air am air.
  Mae system ffeiliau nad yw ei fformat yn storio priodoleddau (FAT32,
  ext4) yn dweud hynny yn y golwg; mae golygydd y modd yn dal i weithio
  yno.
- `t` — tagio neu ddad-dagio cofnod a ddewiswyd y panel ffeiliau a
  symud rhes i lawr; mae pwyso dro ar ôl tro felly'n tagio rhes o
  gofnodion. Mae cofnodion wedi'u tagio yn cario `*`.
- `T` — tagio yn ôl patrwm: glob (`*`, `?`, `[...]`) a gymherir â'r
  enwau gweladwy, neu amrediad — `size:MIN..MAX` (beitiau, ôl-ddodiaid
  `K`/`M`/`G`/`T`, a gellir gadael y naill derfyn neu'r llall yn agored:
  `size:1M..`, `size:..64K`) neu `date:YYYY-MM-DD..YYYY-MM-DD` (dyddiad
  addasu, gan gynnwys y dyddiad olaf; nid yw cofnod nad yw ei storfa'n
  cadw amser byth yn cydweddu â dyddiad). Ychwanegir pob cydweddiad at y
  set a dagiwyd; nid yw patrwm neu amrediad camffurfiedig yn tagio dim,
  ac mae'n dweud pam.
- `i` — gwrthdroi'r tagiau dros y cofnodion gweladwy.
- `C` — clirio pob tag.
- `f` — hidlo'r panel ffeiliau â glob enw ffeil, a gymhwysir yn fyw wrth
  ei deipio. Mae `Enter` yn cadw'r hidl (a ddangosir yn y llinell
  statws), ac mae `Esc` yn adfer yr hidl fel yr oedd. Nid yw patrwm na
  ellir ei grynhoi yn cuddio dim — mae'r llinell statws yn ei nodi â
  `(bad pattern)` yn lle hynny. Mae gwagio'r patrwm yn clirio'r hidl.
- `/` — chwilio'r gangen o dan y cyfeiriadur â ffocws yn ôl glob enw
  ffeil, a gymherir â llwybr pob ffeil yn gymharol â'r gangen. Daw'r
  canlyniadau i'r golwg fflat wrth iddynt gael eu canfod.
- `F` — chwilio cynnwys ffeiliau am destun llythrennol, heb ystyried
  priflythrennau, wedi'i ffrydio mewn ffenestri cyfyngedig (caiff
  cydweddiad sy'n croesi ffin ddarllen ei ganfod serch hynny). Gyda
  chofnodion wedi'u tagio, mae'r chwiliad yn cwmpasu'r set a dagiwyd
  (cyfeiriaduron a dagiwyd yn atblygol); fel arall mae'n cwmpasu'r gangen
  â ffocws. Mae pob rhes canlyniad yn cario ei nifer o gydweddiadau;
  adroddir am ffeil y mae ei chynnwys yn edrych yn ddeuaidd fel
  cydweddiad deuaidd — ni ddangosir ei beitiau byth. Rhestrir ffeil na
  ellir ei darllen yn adroddiad y daith, ac ni chaiff byth ei hepgor yn
  dawel.
- `u` — cyfrif defnydd disg o dan y cyfeiriadur â ffocws: ffeiliau,
  beitiau a chyfeiriaduron, wedi'u cerdded fesul cam yn y cefndir.
  Mae `Esc` yn canslo gan gadw'r ffigurau a gyfrifwyd hyd hynny.
- `v` — fflatio'r gangen o dan y cyfeiriadur â ffocws: un rhestr o
  bob ffeil oddi tani, yn llenwi fesul tudalen (mae `Space` yn llwytho'r
  dudalen nesaf). Yn y golwg, mae `t`/`T`/`i`/`C` yn tagio ei rhesi,
  mae `c`/`m`/`d` yn rhedeg gweithredoedd swp dros y set a dagiwyd, mae
  `Enter` yn neidio i gyfeiriadur y rhes a ddewiswyd yn y paneli (gyda'r
  cyrchwr yn glanio ar ei chofnod), ac mae `Esc` yn dychwelyd i'r paneli.
  Tra bo taith neu chwiliad yn dal i redeg, mae `Esc` yn ei atal yn
  gyntaf, gan gadw'r rhesi a ganfuwyd hyd hynny. Enwir y rhesi yn
  gymharol â'r gangen a fflatiwyd. Mae canlyniadau chwilio (`/`, `F`)
  yn llenwi'r un golwg hon, felly gellir tagio a gweithredu ar eu rhesi
  yn union fel rhestr wedi'i fflatio.
- `H` — dangos/cuddio cofnodion cudd (enwau â dot) yn y ddau banel.
- `.` — ailadrodd y weithred ffeil olaf ar y dewisiad cyfredol: mae copi
  neu symud yn mynd i'r un cyfeiriadur cyrchfan eto, ac mae dileu yn
  gofyn eto yn ôl gosodiad y cadarnhad. Os na fu gweithred eto, mae'r
  fysell yn dweud hynny.
- `V` — rhestru'r cyfrolau a osodwyd: mae pob rhes yn dangos y pwynt
  gosod, math y system ffeiliau, a'r beitiau rhydd/cyfanswm pan fo'r
  gyfrol yn eu hadrodd. Mae `Enter` yn ail-wreiddio'r sesiwn gyfan ar y
  gyfrol a ddewiswyd; mae `Esc` yn cau'r rhestr. Pan na adroddir am
  unrhyw gyfrol, mae'r fysell yn dweud hynny.
- `S` — y ddewislen gosodiadau: mae `1` yn toglo'r cadarnhad dileu
  unigol, a `2` y cadarnhad dileu swp. Caiff newidiadau eu cadw yng
  ngosodiadau'r rhaglen hon ei hun, na all neb ond hi eu darllen, ac
  fe'u llwythir yn y cychwyn nesaf; pan na ellir cyrraedd y gwasanaeth
  gosodiadau, dim ond am y sesiwn y maent yn para ac mae'r ddewislen yn
  dweud pam.
- `?` — dangos y cymorth hwn dros y paneli; mae unrhyw fysell yn ei gau.
- `q` — gadael, gan adfer y derfynell.

Tra bo cofnodion wedi'u tagio, mae `c`, `m` a `d` yn gweithredu ar y
set gyfan a dagiwyd yn hytrach na'r dewisiad: mae `c`/`m` yn gofyn am
gyfeiriadur cyrchfan presennol y mae'r cofnodion yn glanio ynddo (mae
`Tab` yn ei gwblhau), ac mae `d` yn cadarnhau'r dileu swp (oni bai bod
y cadarnhad hwnnw wedi'i ddiffodd yn y ddewislen gosodiadau). Prosesir
y cofnodion yn nhrefn eu tagio; nid yw cofnod a fethodd byth yn atal y
gweddill, mae'r adroddiad terfynol yn cyfrif yr hyn a lwyddodd, ac mae
sgrin adroddiad yn enwi pob methiant — nid yw swp byth yn rhannol yn
dawel. Caiff cofnodion a lwyddodd eu dad-dagio; erys methiannau wedi'u
tagio ar gyfer ailgynnig.

Pan fyddai copïo neu symud yn trosysgrifo ffeil bresennol, mae'r
sesiwn yn gofyn fesul ffeil: mae `o` yn trosysgrifo, mae `s` yn hepgor
(erys ffynhonnell a hepgorwyd yn ei lle), ac mae `c` yn canslo'r camau
sy'n weddill — mewn swp, mae canslo'n gollwng pob cofnod sy'n weddill
— erys yr hyn a gymhwyswyd eisoes, ac mae'r adroddiad
terfynol yn dweud beth ddigwyddodd. Mae methiant hanner ffordd drwy
gopïo yn tynnu'r gyrchfan hanner-ysgrifenedig ac yn dangos gwall y
cnewyllyn; nid oes dim byth yn esgus bod yn gopi cyflawn. Caiff pob
gweithred ei hawdurdodi gan y cnewyllyn — ymddengys gwrthodiad air am
air ar y llinell negeseuon heb i ddim newid.
Mae copi'n cadw tyllau ffeil a'i phriodoleddau estynedig; caiff
unrhyw briodoledd na all y gyrchfan ei chadw ei henwi ar y llinell
negeseuon pan ddaw'r weithred i ben.

Mae cyfeiriadur a gopïir neu a symudir ar gyfeiriadur o'r un enw yn cael ei
gyfuno ag ef: mae'r hyn sydd ynddo yn ymuno â'r cyfeiriadur sydd yno eisoes,
a gofynnir am bob un o'i ffeiliau sydd yno eisoes yn yr un modd.

Mae'r pennyn ystadegau disg ar y brig yn dangos y llwybr a restrwyd,
beitiau rhydd/cyfanswm y gyfrol sylfaenol (pan all y gwasanaeth
gwybodaeth system eu hadrodd), nifer yr eitemau gweladwy, a — tra bo
rhywbeth wedi'i dagio — nifer y cofnodion a dagiwyd. Mae'r band statws
o dan y ffenestri yn dangos y ffenestr weithredol (Tree neu Files), y
drefn didoli, a yw cofnodion cudd yn cael eu dangos, yr hidl enw ffeil
gweithredol, a nifer y cofnodion a dagiwyd gyda'u cyfanswm beitiau. Mae
ffeil nad yw ei fformat storio yn cadw amser addasu yn dangos `-` yng
ngholofn yr amser.

Pan fo'r panel ffeiliau yn hepgor cofnodion cudd, mae'r sesiwn hefyd yn
nodi'r hepgor ar y ffrwd wybodaeth safonol (fd 3) — un cofnod cynghori
fesul newid, y gellir ei ddal â `fstree 3>info.jsonl`; nid effeithir ar
y sesiwn ei hun.

Y gwylwyr: mae `Enter` ar ffeil reolaidd yn ei hagor yn ddarllen-yn-unig
mewn gwyliwr sgrin lawn. Mae ffeil weithredadwy a adnabyddir — delwedd
`rxe`, ELF 64-did, modiwl wasm, neu faniffest llofnodedig ar ei ben ei
hun — yn agor yn y **gwyliwr dadosodiad**; mae ffeil y mae ei beitiau
cyntaf yn rhydd o NUL ac yn UTF-8 dilys yn agor yn y **tudalennwr
testun**; mae popeth arall yn agor yn y **dymp hecs**. Mae `o` yn
diystyru'r dewis. Mae `x` yn newid y gwyliwr agored i'r dymp hecs yn yr
un man, `t` i'r tudalennwr testun (wedi'i alinio â dechrau'r llinell
sy'n cynnwys y gwrthbwyso a ddangosir) a `d` i'r gwyliwr dadosodiad
(mae ffeil nad yw unrhyw fformat cynhwysydd yn ei hawlio yn gofyn am set
gyfarwyddiadau ac yn cael ei datgodio fel darn crai o'r man cyfredol);
mae `q` neu `Esc` yn dychwelyd i'r paneli.

Mae'r gwylwyr yn tudalennu drwy'r ffeil mewn ffenestri cyfyngedig — ni
chedwir y ffeil yn gyfan yn y cof byth, felly mae ffeil o unrhyw faint
(ymhell dros 4 GiB) yn tudalennu'n gywir — ac maent yn rhannu'r bysellau:

- `Up`/`Down` neu `k`/`j` — un rhes; `PageUp`/`PageDown`, `b`/`Space` —
  un dudalen; `Home`/`End` — y dechrau / y dudalen olaf.
- `g` — mynd i fan: rhif llinell yn cyfrif o 1 yn y tudalennwr testun,
  gwrthbwyso beit (degol neu hecs gyda `0x`) yn y dymp hecs, cyfeiriad
  yn y gwyliwr dadosodiad.
- `/` — chwilio ymlaen o'r man cyfredol; mae `n` yn ailadrodd y
  chwiliad olaf heibio i'w drawiad blaenorol. Mae'r sgan yn rhedeg yn y
  cefndir fesul cam cyfyngedig — mae'r sesiwn yn dal i ymateb dros ffeil
  enfawr, mae'r llinell statws yn dweud `searching…`, ac mae `Esc` yn ei
  atal yn ei unfan.
- `?` — y cymorth hwn; mae unrhyw fysell yn ei gau.

Mae'r **tudalennwr testun** yn datgodio UTF-8 gydag amnewidiad gweladwy
ar gyfer beitiau annilys, yn ehangu tabiau, ac yn dangos pob beit rheoli
arall fel `·` gweladwy — ni anfonir cynnwys ffeiliau byth i'r derfynell
fel dilyniannau dianc crai. Mae llinellau hir yn lapio yn ddiofyn; mae
`w` yn toglo'r lapio (heb lapio, caiff y gynffon ei thorri wrth yr ymyl
dde). Mae `/` yn chwilio am destun llythrennol, heb ystyried
priflythrennau, ar draws ffiniau darllen. Mae'r llinell statws yn dangos
y man fel rhif llinell; ar ôl neidio o'r dymp hecs nid yw'r rhif llinell
yn hysbys (ni thalwyd am y cyfrif) nes i neidio â `g` ei angori eto.

Mae'r **dymp hecs** yn dangos y gosodiad clasurol — colofn wrthbwyso,
un beit ar bymtheg fel parau hecs, a cholofn ASCII gyda beitiau
anargraffadwy fel `.`. Mae'r golofn wrthbwyso mor llydan ag sydd ei
angen ar y ffeil (o leiaf wyth digid hecs). Mae `/` yn chwilio am destun
llythrennol (heb ystyried priflythrennau) neu, wedi'i sillafu fel `0x`
ac yna parau beitiau hecs (`0xdeadbeef`), am ddilyniant beitiau union.

Mae'r **gwyliwr dadosodiad** yn agor ar dudalen grynodeb: fformat y
cynhwysydd, ei set gyfarwyddiadau a'i bwynt mynediad, ei ranbarthau
(cyfeiriad, rhychwant yn y ffeil, maint yn y cof, caniatâd), a'i nifer
o symbolau; mae maniffest llofnodedig ar ei ben ei hun yn rhestru yn
hytrach ei fersiwn ABI a'r galluoedd y mae'n gofyn amdanynt. Mae delwedd
`rxe` yn cario ei maniffest wrth ei hochr, byth wedi'i fewnosod, ac mae'r
crynodeb yn dweud hynny. Mae `Up`/`Down` yn symud dros resi'r rhanbarthau
ac mae `Enter` yn agor dadosodiad y rhanbarth cod a ddewiswyd (dangosir
rhanbarth data fel hecs ar ei beitiau ffeil). Mae cynhwysydd nad yw'n
enwi unrhyw set gyfarwyddiadau — mae delwedd `rxe` yn rhedeg ar ba
beiriant bynnag sy'n ei llwytho — yn gofyn unwaith: `x` x86-64,
`a` aarch64, `r` riscv64, `w` wasm.

Mae'r panel cod yn dangos un cyfarwyddyd fesul rhes — cyfeiriad, beitiau
amgodio, cofair, operandau — gyda llinellau label `<symbol>:` a
thargedau cangen wedi'u symboleiddio (`<main+0x8>`) lle mae'r
cynhwysydd yn enwi symbolau. Caiff cyfarwyddiadau eu datgodio fesul
sgrinaid, byth y deuaidd cyfan ymlaen llaw. Mae `g` yn neidio i
gyfeiriad, mae `/` yn chwilio testun y cyfarwyddiadau, mae `End` yn
cerdded i ddiwedd y rhanbarth yn y cefndir (mae `Esc` yn atal y daith),
ac mae `I` yn ail-ddatgodio â set gyfarwyddiadau arall. Mae `Esc` yn
dychwelyd i'r dudalen grynodeb.

Mae pob datgodio cynhwysydd a chyfarwyddiadau yn rhedeg mewn proses
gynorthwyol gaeedig (blwch tywod y dosranwyr), byth yn y rheolwr ffeiliau
ei hun: gall ffeil weithredadwy faleisus chwalu'r broses gynorthwyol, nid
y sesiwn. Mae ffeil y mae'r datgodiwr yn ei gwrthod — wedi'i chamffurfio,
neu'n rhy fawr i'w throsglwyddo i'r blwch tywod — yn disgyn yn ôl i'r
dymp hecs gyda hysbysiad un llinell.

Mae darlleniad y mae'r cnewyllyn yn ei wrthod ar ganol gwylio yn cau'r
gwyliwr ac yn dangos y gwall ar y llinell negeseuon — ni ddangosir
cynnwys hen byth fel pe bai'n fyw.

## OPTIONS

- `directory` — y cyfeiriadur y mae'r sesiwn yn dechrau ynddo; y
  rhagosodiad yw'r golwg gwraidd `/`.
- `-h`, `-?` — argraffu ffurf fer y ddogfen hon a gadael.

## EXIT STATUS

- `0` — daeth y sesiwn i ben drwy `q` y defnyddiwr.
- `1` — ni ellid rhestru'r cyfeiriadur cychwynnol, neu fethodd llwybr y
  derfynell. Mae llinell orchymyn na ddeallwyd hefyd yn gorffen â `1`.

## SEE ALSO

ls, cp, mv, rm, mkdir, chmod, du, df, find
