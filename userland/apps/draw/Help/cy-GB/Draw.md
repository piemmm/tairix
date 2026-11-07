## NAME

Draw — golygydd lluniadau fector

## SYNOPSIS

`Draw`

## DESCRIPTION

Yn lluniadu ac yn golygu lluniadau fector mewn ffenestr ar y bwrdd gwaith:
llwybrau, petryalau, elipsau, sêr a pholygonau, llinellau, cromliniau llawrydd
a thestun, mewn grwpiau a haenau, ar dudalen. O'i gychwyn â dogfen — o'r
rheolwr ffeiliau, o'r bwrdd gwaith, neu drwy ollwng ffeil ar ei eicon yn y bar
eiconau — mae'n agor ffenestr arni. O'i gychwyn ar ei ben ei hun mae'n agor
lluniad newydd ar dudalen A4. Mae pob dogfen yn ffenestr o'r un rhaglen; mae
cau'r olaf yn ei gadael ar y bar eiconau, ac mae rhes Gadael ei ddewislen
eicon yn ei gorffen.

Mae Draw yn agor ac yn cadw SVG. Mae lluniad yn cadw ei dudalen a'i unedau,
ei haenau, ei grwpiau a'i docynnau, pob llwybr, siâp a thestun, lliwiau plaen
a graddiannau llinol a rheiddiol, didreiddedd pob gwrthrych, toriadau,
uniadau, pennau a phennau saeth, y canllawiau a'r grid, a theitl y lluniad.
Mae ffeil sy'n dal yr hyn na all lluniad ei gadw — effeithiau hidlo, masgiau,
llenwadau patrwm, marcwyr, lluniau mewnosod, arddulliau testun cymysg,
animeiddio neu sgriptiau yn eu plith — yn agor yn ddarllen yn unig, gyda'r
band statws yn enwi'r hyn y mae'n ei ddal, ac mae Cadw yn gofyn am enw newydd
fel na chaiff y gwreiddiol byth ei drosysgrifo. Mae Cadw copi yn ysgrifennu'r
lluniad fel PDF, fel PNG — y dudalen yn ei maint ei hun, hyd at 8192 picsel ar
ei hochr hwyaf — neu fel SVG, gan adael ffeil y ddogfen ei hun fel y mae.

Mae'r blwch offer yn rhedeg i lawr ochr chwith y ffenestr, yr archwiliwr i
lawr ei hochr dde, a'r band statws ar hyd ei gwaelod: mae'n dangos lle'r
pwyntydd a maint y dewis yn unedau'r dudalen, ac yn dweud beth wnaeth y
gorchymyn olaf. Ar draws y brig, mae'r bar yn dweud sut i ddefnyddio'r offeryn
mewn llaw ac yn dangos y chwyddo: mae pwyso'r chwyddo yn agor dewislen o
chwyddiadau. Mae prenmesurau yn unedau'r dudalen yn rhedeg ar hyd brig ac ochr
chwith y cynfas, a barrau sgrolio ar hyd ei ochr dde a'i waelod. Mae oedi dros
fotwm offeryn yn ei enwi, gyda'r fysell sy'n ei ddewis.

Yr offer, a'r bysellau sy'n eu dewis, yw: dewis (`V`), nodau (`A`),
petryal (`R`), elips (`E`), seren a pholygon (`S`), llinell (`L`), pin
(`B`), pensil (`P`), testun (`T`), diferydd (`I`), chwyddo (`Z`) a llaw
(`H`).

Mae'r offeryn dewis yn cymryd gwrthrych â gwasgiad, grŵp yn gyfan, ac mae
Shift yn ychwanegu gwrthrych at y dewis neu'n ei dynnu allan. Mae gwasgiad ar
gynfas gwag yn llusgo ffrâm sy'n dewis yr hyn sy'n gorwedd yn gyfan gwbl o'i
mewn, neu ag Alt bopeth y mae'n ei gyffwrdd. Mae llusgo'r dewis yn ei symud,
ar hyd un echelin â Shift, ac ag Alt mae'n symud copi. Mae ei wyth dolen yn ei
raddio o'r ddolen gyferbyn, cornel â Shift yn cadw ei gyfrannau; mae llusgo
ychydig y tu allan i gornel yn ei droi o amgylch ei ganol, â Shift mewn camau o
15 gradd. Mae'r bysellau saeth yn ei symud un picsel, a deg â Shift.

Mae'r offeryn nodau yn dangos nodau'r llwybr a ddewiswyd a dolenni ei
gromliniau: llusgwch nod i'w symud, neu ddolen i blygu ei chromlin; mae dolen
arall nod llyfn yn dilyn yn unol. Caiff petryalau, elipsau, sêr a llinellau eu
lluniadu drwy lusgo: mae Shift yn gwneud yr ochrau'n gyfartal — sgwâr,
cylch, llinell ar luosrif o 15 gradd — ac mae Alt yn lluniadu allan o'r man lle
dechreuodd y llusgo. Caiff seren ei lluniadu allan o'i chanol; gosodir ei
phwyntiau, ei radiws mewnol ac a yw'n bolygon yn nhab Siâp yr archwiliwr, fel
corneli petryal newydd.

Mae'r pin yn gosod llwybr nod wrth nod: cliciwch am gornel, neu llusgwch am
nod llyfn a'i gromlin. Mae clic ar y nod cyntaf yn cau'r llwybr, ac mae
Enter, neu ddewis offeryn arall, yn ei orffen yn agored; mae `Delete` neu
`Backspace` yn cymryd y nod olaf yn ôl, ac mae Escape yn gollwng y llwybr. Mae'r
pensil yn lluniadu'n llawrydd, a chaiff ei linell ei ffitio â chromliniau llyfn
pan gaiff ei ollwng. Mae'r offeryn testun yn dechrau testun lle cliciwyd, neu'n
golygu'r testun a gliciwyd: teipiwch, mae Enter yn dechrau llinell, mae'r
bysellau saeth, Home ac End yn symud y caret, ac mae Escape, neu glic mewn man
arall, yn gorffen y teipio fel un newid; caiff testun a adawyd yn wag ei
dynnu. Mae'r diferydd yn rhoi i'r dewis, ac i'r hyn a luniedir nesaf, arddull
y gwrthrych a gliciwyd. Mae'r offeryn chwyddo yn chwyddo i mewn lle cliciwyd,
allan ag Alt, ac mae blwch a lusgir ag ef yn llenwi'r ffenestr. Mae'r llaw yn
llusgo'r cynfas, fel y mae llusgo â'r botwm canol neu â Bwlch wedi'i ddal, pa
offeryn bynnag.

Mae'r olwyn yn sgrolio'r cynfas, ar draws â Shift; â Ctrl mae'n chwyddo o
amgylch y pwyntydd, fel y mae pinsio ar bad cyffwrdd neu sgrin gyffwrdd. Mae
Golwg ▸ Ffitio'r dudalen a Ffitio'r dewis yn eu ffitio yn y ffenestr, ac mae
Maint gwirioneddol yn dangos y lluniad ar 100 y cant; mae'r chwyddo'n rhedeg o
1 y cant i 25600.

Mae gan yr archwiliwr bedwar tab. Mae Arddull yn gosod llenwad a strôc y
dewis — ac, heb ddim wedi'i ddewis, yr hyn a luniedir nesaf: dewiswch Llenwad
neu Strôc, yna Dim neu liw o'r dewisydd lliw, sy'n dal sgwâr dirlawnder a
gwerth wrth stribed o liwiau, olwyn neu lithryddion, a rhifau a didreiddedd y
lliw; yna lled y strôc, ei huniadau, meitr, crwn neu befel, ei phennau, fflat,
crwn neu sgwâr, ei thoriadau, solet, toredig, dotiog neu doriad-dot, y pennau
saeth ar ei dechrau a'i diwedd — mae pob gwasgiad yn camu drwy ddim, triongl,
saeth, cylch a diemwnt —, y rheol llenwi, nid-sero neu eilrif-odrif, a
didreiddedd pob gwrthrych a ddewiswyd. Mae Haenau yn rhestru'r haenau, yr uchaf
yn gyntaf: pwyswch un i luniadu iddi, a'i botwm Yn dangos i'w dangos neu'i
chuddio; uwchben y rhestr, mae Newydd yn ychwanegu haen, mae Tynnu yn tynnu'r
haen mewn llaw, mae Cloi yn ei chloi rhag newid, ac mae Codi a Gostwng yn ei
symud. Mae Gwrthrych yn gosod y dewis wrth ei ochr chwith a'i frig ac yn ei
raddio i led ac uchder, yn ei alinio — wrth ymylon chwith, canolau neu ymylon
de, neu wrth y brig, y canol neu'r gwaelod — ac yn taenu tri gwrthrych neu fwy
yn gyfartal ar draws neu i lawr. Mae gwrthrychau'n alinio o fewn eu dewis; un
gwrthrych, neu unrhyw nifer ag Alinio i'r dudalen ymlaen, o fewn y dudalen. Mae
Siâp yn dal corneli petryal, pwyntiau a radiws mewnol seren ac a yw'n bolygon,
a theulu'r testun — sans, serif neu unlled —, ei faint, trwm, italig a'i
aliniad ar ei darddbwynt. Mae tab sy'n dalach na'r ffenestr yn sgrolio. Caiff
lliw sy'n cael ei ddewis, lled a deipir neu a gamir ac didreiddedd eu dangos ar
y cynfas wrth iddynt newid, a dônt yn un newid i'w ddadwneud pan setlant; mae
Tab yn symud i'r maes nesaf a Shift+Tab i'r un cynt.

Mae llusgo o brenmesur yn gosod canllaw ar draws neu i lawr y cynfas; llusgwch
ganllaw i'w symud, neu'n ôl ar brenmesur i'w dynnu. Mae Golwg ▸ Grid yn dangos
grid y dudalen. Tra bo Snapio ymlaen, mae'r pwyntydd yn snapio wrth luniadu
siâp, gosod nod â'r pin neu symud neu raddio'r dewis: at ganllawiau, at ymylon
a chanol y dudalen, ac at y grid tra bo'n dangos. Mae Golwg ▸ Amlinellau yn
dangos pob gwrthrych fel ei amlinell, sy'n gwneud lluniad llawn yn ddarllenadwy.
Mae'r ddewislen Tudalen yn gosod papur y dudalen — A4, A3, A5, A6, A2, A1, A0,
B5, B4, US Letter, US Legal, Tabloid, Executive, dau faint sgrin ac eicon —, yn
ei throi rhwng portread a thirlun, yn rhoi cefndir gwyn iddi neu ddim, ac yn
gosod ei hunedau: picseli, pwyntiau, picâu, milimetrau, centimetrau neu
fodfeddi.

Mae Gwrthrych ▸ Grwpio yn casglu'r dewis yn grŵp ac mae Dadgrwpio yn ei
ryddhau; mae Dod i'r blaen, Codi, Gostwng ac Anfon i'r cefn yn aildrefnu
gwrthrychau o fewn eu haen; mae Fflipio a Throi yn adlewyrchu'r dewis neu'n ei
droi chwarter tro. Mae Cloi a Chuddio yn rhoi gwrthrychau allan o gyrraedd, ac
mae Datgloi'r cyfan a Dangos y cyfan yn yr haen yn dod â rhai'r haen mewn llaw
yn ôl. Mae'r ddewislen Llwybr yn uno'r siapiau a ddewiswyd yn un (Uniad), yn
torri'r rhai uwchben allan o'r isaf (Gwahaniaeth), yn cadw lle maent yn
gorgyffwrdd (Croestoriad) neu lle nad ydynt (Allgau), yn troi siapiau a
thestun yn llwybrau y gellir golygu eu nodau, ac yn troi strociau yn
amlinellau wedi'u llenwi. Caiff y rhain eu cyfrifo ar wahân i'r ffenestr, sy'n
aros yn rhydd i'w defnyddio yn y cyfamser; mae lluniad a newidiwyd cyn i'r
gwaith lanio yn cadw ei newid, ac ni chaiff y gwaith ei gymhwyso.

Mae pob newid yn un cam i'w ddadwneud, ac mae Dadwneud ac Ailwneud yn enwi'r
cam a gymerant; cedwir hyd at 1000 o gamau, llai tra bo cof yn brin. Mae Copïo
yn rhoi'r dewis ar y clipfwrdd fel SVG, ac mae Gludo yn ychwanegu'r hyn sydd ar
y clipfwrdd at yr haen mewn llaw, yn ei le ar y dudalen, wedi'i ddewis.

Nid yw Draw yn dal unrhyw allu dros y system ffeiliau. Mae'n golygu dim ond y
ffeil a roddwyd iddo: caiff ffeil y caiff y defnyddiwr ei newid ei rhoi'n
ysgrifenadwy ac mae Cadw yn ei hysgrifennu'n ôl, ac mae unrhyw un arall yn agor
yn ddarllen yn unig ac mae Cadw yn gofyn ble i'w chadw. Caiff dogfennau, a
lluniadau a ludir o'r clipfwrdd, eu darllen mewn proses waith ar wahân heb
unrhyw gyrhaeddiad o gwbl, fel na all ffeil elyniaethus gyffwrdd â dim y gall
Draw ei gyrraedd.

Mae pwyso botwm eilaidd (de) y llygoden unrhyw le yn y ffenestr yn agor ei
dewislen: Ffeil, Tudalen, Golygu, Gwrthrych, Llwybr, Haen, Golwg ac Offer, pob
un yn is-ddewislen. Nid oes gan y ffenestr far dewislen. Mae cau ffenestr, neu
adael, gyda newidiadau heb eu cadw yn gofyn yn gyntaf. Mae cau ffenestr â'r
botwm eilaidd ar ei botwm cau hefyd yn dangos ei ffeil yn y rheolwr ffeiliau,
yn y ffolder sy'n ei dal.

* `Ctrl+N` — ffenestr newydd; `Ctrl+O` — agor ffeil
* `Ctrl+S` — cadw; `Ctrl+Shift+S` — cadw fel; `Ctrl+Shift+E` — cadw copi fel PDF, PNG neu SVG
* `Ctrl+W` — cau'r ffenestr
* `Ctrl+Z` — dadwneud; `Ctrl+Shift+Z` neu `Ctrl+Y` — ailwneud
* `Ctrl+X`, `Ctrl+C`, `Ctrl+V` — torri, copïo, gludo; `Ctrl+D` — dyblygu
* `Delete` neu `Backspace` — dileu'r dewis
* `Ctrl+A` — dewis y cyfan; `Ctrl+Shift+A` — dewis dim; `Escape` — dewis dim, neu ollwng y llwybr sy'n cael ei osod
* `Ctrl+G` — grwpio; `Ctrl+Shift+G` — dadgrwpio
* `Page Up` / `Page Down` — codi neu ostwng; `Home` / `End` — dod i'r blaen neu anfon i'r cefn
* `Ctrl+[` / `Ctrl+]` — troi i'r chwith neu i'r dde
* `Ctrl+L` — cloi; `Ctrl+Shift+L` — datgloi'r cyfan yn yr haen
* `Ctrl+H` — cuddio; `Ctrl+Shift+H` — dangos y cyfan yn yr haen
* `Ctrl+Shift+C` — gwrthrych yn llwybr
* `Ctrl+Shift+N` — haen newydd; `Ctrl+Shift+Page Up` / `Ctrl+Shift+Page Down` — codi neu ostwng yr haen
* `Ctrl+=` neu `Ctrl++` / `Ctrl+-` — chwyddo i mewn neu allan; `Ctrl+1` — maint gwirioneddol; `Ctrl+0` — ffitio'r dudalen
* `Ctrl+'` — dangos neu guddio'r grid; `%` — snapio neu beidio
* `V`, `A`, `R`, `E`, `S`, `L`, `B`, `P`, `T`, `I`, `Z`, `H` — yr offer, yn eu trefn
* bysellau saeth — symud y dewis un picsel; â `Shift`, deg
* `Space` — wedi'i ddal, llusgo'r cynfas ag unrhyw offeryn
* `Ctrl` + olwyn — chwyddo o amgylch y pwyntydd; `Shift` + olwyn — sgrolio ar draws

## OPTIONS

`-h`, `-?`, `--help`
: Ysgrifennu'r help hwn i'r allbwn safonol a gadael.

## EXIT STATUS

Sero ar ôl Gadael. Heb fod yn sero pan wrthodwyd sianel y ffenestr, blwch post
y digwyddiadau, neu sesiwn y bwrdd gwaith; nodir y rheswm ar y ffrwd gwall
safonol.
