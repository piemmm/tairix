## NAME

Paint — golygydd lluniau a chorluniau graffigol

## SYNOPSIS

`Paint`

## DESCRIPTION

Yn paentio ac yn golygu lluniau mewn ffenestr ar y bwrdd gwaith, picsel
wrth bicsel neu â brwshys a siapiau. O'i gychwyn â dogfen — o'r rheolwr
ffeiliau, o'r bwrdd gwaith, neu drwy ollwng ffeil ar ei eicon yn y bar
eiconau — mae'n agor ffenestr arni. O'i gychwyn ar ei ben ei hun mae'n agor
llun gwyn newydd. Mae pob dogfen yn ffenestr o'r un rhaglen; mae cau'r olaf
yn ei gadael ar y bar eiconau, ac mae rhes Gadael ei ddewislen eicon yn ei
gorffen.

Mae'n agor pob fformat llun y mae'r system yn ei ddarllen: PNG, JPEG, GIF,
BMP, TIFF, WebP, eiconau Windows, ffeiliau corluniau RISC OS ac OpenRaster.
Mae'n ysgrifennu PNG, JPEG, GIF, BMP, TIFF, ffeiliau corluniau ac
OpenRaster; caiff llun a ddarllenwyd o unrhyw fformat arall, neu o ffeil sy'n
dal mwy na'i llun, fel proffil lliw, ei gadw fel ffeil newydd. Mae Llun
newydd yn gofyn yn gyntaf i ba fformat y mae'r llun, ac yn cynnig y lliwiau
y mae'r fformat hwnnw'n eu dal. Mae Cadw fel yn gofyn y fformat a'i
osodiadau ei hun — ansawdd JPEG, a yw GIF wedi'i gydblethu, cywasgiad TIFF —
ac yn dweud beth na all y fformat ei gadw, cyn gofyn ble. Cedwir palet lle
bynnag y mae'r fformat yn dal un, a dwysedd llun hefyd.

Gall llun gael ei wneud o haenau, yr isaf yn gyntaf, pob un ag enw,
didreiddedd ac a yw'n dangos; mae paentio'n glanio ar un haen ar y tro, ac
mae'r ffenestr yn eu dangos wedi'u gosod ynghyd. Mae OpenRaster yn cadw'r
haenau; caiff pob fformat arall hwy wedi'u gosod ynghyd. Mae'r ddewislen
Haenau yn ychwanegu, copïo, dileu, codi a gostwng haenau, yn uno un i lawr
ar yr un oddi tani ac yn gwastatáu'r llun, ac mae ei Phriodweddau haen yn
ailenwi haen ac yn gosod faint ohoni sy'n dangos. Mae addasiadau, llenwadau
a strociau'n newid yr haen y paentir arni; mae troeon, fflipiau, newid maint
a thocio'n newid pob haen. Un haen sydd gan lun â phalet.

Mae ffeil corluniau'n dal unrhyw nifer o gorluniau, pob un â'i enw, ei fodd
sgrin, ei balet a'i fasg. Golygir pob dyfnder fel y'i storiwyd: 2, 4, 16 a
256 o liwiau a miliynau o liwiau. Mae corlun heb balet ei hun yn dangos
lliwiau bwrdd gwaith RISC OS — am 16 lliw, lliw n yw lliw Wimp n; am 2
liw, lliwiau Wimp 0 a 7; am 4 lliw, lliwiau Wimp 0, 2, 4 a 7; am 256 o
liwiau, trefniant arlliwiau RISC OS — byth palet PC. Dangosir corlun y mae
ei bicseli'n dalach na'u lled, fel ym modd 12, felly. Cedwir corlun na all y
golygydd hwn ei ddarllen, fel un CMYK, yn union fel yr oedd a'i ysgrifennu'n
ôl heb newid. Mae'r ddewislen Corluniau'n mynd at gorluniau, yn eu
hychwanegu, eu copïo, eu hailenwi, eu dileu a'u haildrefnu. Mae TIFF yn dal
unrhyw nifer o dudalennau, ac iddo ef mae'r ddewislen Tudalennau'n mynd at
dudalennau, yn eu hychwanegu, eu copïo, eu dileu a'u haildrefnu.

Mae'r prif fotwm (chwith) yn paentio â'r prif liw a'r botwm canol â'r
ail liw; o ddal Alt, codir lliw yn lle hynny, fel y mae'r haenau'n ei
ddangos. Mae'r blwch offer, yn y cwarel Offer, yn dal yr offer mewn dwy
golofn: dewis, pensil, brwsh, brwsh aer, rhwbiwr, clonio, llenwi, graddiant,
piped, testun, llinell, petryal, elips, polygon, tocio, llaw a chwyddo. Mae'r
bar ar draws y brig yn enwi'r offeryn sydd ar waith ac yn dal ei osodiadau —
maint, caledwch, didreiddedd, llif a bylchau brwsh, goddefiant llenwad, siâp
graddiant, maint y testun, corneli petryal — wedi'u teipio neu eu camu â'r
bysellau saeth, a'r botymau sy'n chwyddo ac yn dangos grid y picseli; mae'r
stribed palet o dan y llun yn dal palet y llun, neu liwiau'r bwrdd gwaith.
Mae'r brwsh aer yn dal i chwistrellu tra caiff ei ddal yn llonydd. O ddal
Shift, lluniadir sgwâr, cylch neu linell ar luosrif o 45 gradd.

Mae cwarelau'n rhedeg i lawr dwy ochr y ffenestr: fel arfer y cwarel Offer ar
y chwith a'r cwarel Lliw ar y dde, gyda'r cwarel Addasiad oddi tano cyn gynted
ag yr agorir addasiad. Mae band main ar ben pob un yn ei enwi, gyda rheolydd
sy'n ei rolio i fyny i'w fand a marc sy'n ei gau; mae Golwg ▸ Cwarelau yn
dangos cwarel caeedig eto, ac mae Ailosod cwarelau yn rhoi pob cwarel yn ôl
fel y mae gan ffenestr newydd hwy. Mae llusgo band yn symud ei gwarel ar ei
ochr neu i'r llall, gan nodi ar y ffordd lle bydd yn glanio; o'i ollwng i
ffwrdd o'r ddwy ochr, neu o'i lusgo allan o'r ffenestr, mae'r cwarel yn
arnofio mewn ffenestr fach ei hun, a symudir gan ei fand ac a gedwir uwchben y
llun, a'i marc yn ei chau; o'i lusgo'n ôl dros ochr, mae'n docio yno eto.

Mae'r offeryn dewis yn nodi petryal, elips, lasŵ llawrydd, polygon a
gliciwyd gornel wrth gornel, neu â'r ffon hud y picseli a gysylltir ag un
drwy liwiau tebyg iddo. Mae dal Shift yn ychwanegu at y dewis, Alt yn tynnu
oddi arno, a'r ddau'n cadw dim ond yr hyn y mae'r ddau'n ei rannu; mae
Pluo'n meddalu ei ymyl. Tra dalier dewis, mae pob offeryn, llenwad ac
addasiad yn gaeth iddo. Mae llusgo y tu mewn iddo'n ei godi a'i symud: mae'n
arnofio nes ei roi i lawr, ac mae ei symud yn un newid i'w ddadwneud. Mae
lluniau a gopïwyd yn teithio drwy'r clipfwrdd fel PNG, ac mae'r hyn a
ludir yn arnofio nes ei roi i lawr.

Mae'r offeryn clonio'n paentio'r hyn sydd mewn man arall yn y llun: Alt a
chlicio ble i gopïo ohono, yna paentio. Mae'r offeryn graddiant yn toddi'r
prif liw i'r ail ar hyd llusgiad, mewn bandiau neu mewn cylchoedd. Mae'r
offeryn testun yn gosod y geiriau a deipiwyd lle cliciwyd; mae Enter yn
dechrau llinell newydd, mae clic arall neu offeryn arall yn eu rhoi i lawr,
ac mae Escape yn eu gollwng. Cliciwyd corneli'r offeryn polygon yn eu tro,
ac mae clic ar y gyntaf neu Enter yn ei gau. Mae'r offeryn tocio'n nodi'r
rhan i'w chadw, mae ei ddolenni'n symud ei ymylon, ac mae Enter yn tocio.
Mae'r llaw yn llusgo'r llun ar draws y ffenestr, fel y mae Space gydag
unrhyw offeryn; mae'r offeryn chwyddo'n chwyddo clic, neu ag Alt yn ei
leihau, ac mae blwch a lusgwyd yn llenwi'r ffenestr.

Mae'r ddewislen Addasu yn agor addasiad yn y cwarel Addasiad, lle mae pob
offeryn, cwarel a dewislen arall yn dal wrth law: disgleirdeb a chyferbyniad,
arlliw a dirlawnder, cydbwysedd lliw, lefelau, cromliniau, cydbwysedd gwyn,
posteru, trothwy, pylu, miniogi, picselu ac ychwanegu sŵn; mae dad-ddirlenwi a
chanfod ymylon, sydd heb osodiadau, yn gweithio ar unwaith. Mae'r llun yn
dangos yr addasiad wrth i'w osodiadau symud, mae Rhagolwg yn ei ddiffodd a'i
ailgynnau i gymharu, mae Ailosod yn rhoi ei osodiadau'n ôl ac mae Gweithredu
yn ei gadw fel un newid i'w ddadwneud; mae paentio, llenwi neu ddewis addasiad
arall yn ei weithredu yn gyntaf. Mae lefelau'n gosod pwyntiau du, llwyd a gwyn
dros histogram o'r haen, i'r holl sianeli gyda'i gilydd neu bob un ar ei
ben ei hun, gyda phipedau sy'n eu cymryd o'r llun ac Awto; mae cromliniau'n
plygu tonau sianel drwy bwyntiau a lusgir dros ei histogram; mae cydbwysedd
gwyn yn gosod tymheredd ac arlliw'r golau, o bicsel niwtral a ddewisir neu
drwy Awto; mae arlliw a dirlawnder yn troi, yn cryfhau ac yn goleuo'r holl
liwiau neu un ystod ohonynt; mae cydbwysedd lliw yn symud y cysgodion, y
canoldonau a'r uchafbwyntiau tuag at goch, gwyrdd neu las, gan gadw eu
goleuni os gofynnir. Mae popeth wedi'i gyfyngu i'r dewis. Ar lun â phalet,
mae addasiad yn newid ei balet, ac ni chynigir y rhai sydd angen lliwiau
cyfagos.

Mae'r cwarel Lliw yn dal y prif liw a'r ail liw a dewisydd lliw i ba un
bynnag ohonynt a ddewisir: mae clic ar liw yn ei ddewis, yna fe'i codir ar
sgwâr o ddirlawnder a gwerth wrth ymyl stribed o arlliwiau, ar olwyn arlliwiau
o amgylch triongl, neu ar lithrydd i bob sianel, ac fe'i teipir mewn RGB, HSV,
HSL, CMYK, Lab, LCh neu fel llwyd, neu drwy ei sillafiad hecsadegol ac, lle
mae'r llun yn dal tryloywder, ei ddidreiddedd. Dangosir lliw Lab neu LCh na
all y sgrin ei ddangos mor agos ag y gellir, a'i nodi. Mae Cyfnewid yn
cyfnewid y ddau liw, mae Ailosod yn eu gwneud yn ddu a gwyn, ac mae Codi yn
cymryd y lliw nesaf a gliciwyd yn y llun. Saif y lliw oedd ganddo wrth ei
ymyl, mae clic yn ei gymryd yn ôl, ac mae'r lliwiau a ddewiswyd ddiwethaf yn
aros oddi tano i'w dewis eto. Ar lun â phalet, ei gofnodion yw'r lliwiau, felly
mae'r dewisydd yn golygu'r palet, ac mae pob golygiad yn un newid i'w
ddadwneud.

Mae Gosodiadau, yn newislen y bar eiconau, yn agor ffenestr osodiadau Paint:
yr offeryn y mae ffenestr newydd yn dechrau ag ef, ac a yw llun yn agor wedi'i
ffitio i'r ffenestr neu ar ei faint go iawn; y maint, y fformat, y lliwiau a'r
cefndir a gynigir gan Llun newydd; bylchau, gwrthbwyso, lliw, didreiddedd ac
arddull y grid — llinellau, llinellau toredig, dotiau neu groesfannau —, a yw
ffenestr newydd yn ei ddangos, snapio ato, a'r chwyddo y dangosir y grid rhwng
picseli ohono; maint a thonau'r bwrdd siec a'r hyn sydd o amgylch y llun; a'r
cwarelau y mae ffenestr newydd yn agor â hwy. Mae newid yn berthnasol i bob
ffenestr ar unwaith ac fe'i cedwir at y tro nesaf; mae Adfer rhagosodiadau yn
eu rhoi i gyd yn ôl. Mae Golwg ▸ Grid yn dangos grid ffenestr. Tra bo'n
weladwy a snapio ymlaen, mae siapiau, dewisiadau a fframiau tocio yn gorchuddio
celloedd cyfan, mae pennau llinell a graddiant a chorneli polygon yn glanio ar
ei groesfannau, ac mae dewis a lusgir yn glanio â'i gornel ar un.

Nid oes gan y rhaglen unrhyw ganiatâd ar y system ffeiliau. Mae'n golygu
dim ond y ffeil a roddwyd iddi. Rhoddir ffeil y caiff y defnyddiwr ei newid
yn ysgrifenadwy, ac mae Cadw'n ei hysgrifennu'n ôl; mae pob un arall yn
ddarllen-yn-unig, ac mae Cadw'n gofyn ble i gadw copi. Datgodir lluniau, a
lluniau a ludwyd o'r clipfwrdd, mewn proses waith ar wahân heb unrhyw
gyrhaeddiad o gwbl, a chaiff pob dogfen un newydd: ni all ffeil elyniaethus
gyffwrdd ag unrhyw beth y gall y rhaglen ei gyrraedd.

Mae pwyso ail fotwm (de) y llygoden unrhyw le yn y ffenestr yn agor ei
dewislen: Torri, Copïo, Gludo, Dewis y cyfan a Dad-ddewis, yna Ffeil,
Golygu, Llun, Haenau, Lliwiau, Addasu, Corluniau neu Dudalennau, Gweld ac
Offer, pob un yn agor ei is-ddewislen ei hun. Nid oes bar dewislen gan y
ffenestr. Mae cau ffenestr neu adael â newidiadau heb eu cadw'n gofyn yn
gyntaf.

* `Ctrl+N` — llun newydd; `Ctrl+O` — agor ffeil
* `Ctrl+S` — cadw; `Ctrl+Shift+S` — cadw fel
* `Ctrl+W` — cau'r ffenestr
* `Ctrl+Z` — dadwneud; `Ctrl+Shift+Z` neu `Ctrl+Y` — ailwneud
* `Ctrl+X`, `Ctrl+C`, `Ctrl+V` — torri, copïo, gludo
* `Ctrl+A` — dewis y cyfan; `Ctrl+D` — dad-ddewis
* `Enter` — gosod dewis sy'n arnofio i lawr, cau polygon neu docio; `Escape` — mynd yn ôl
* `Delete` — clirio'r dewis; `Alt+Backspace` — ei lenwi â'r prif liw
* `Ctrl+Shift+X` — tocio i'r dewis
* `Ctrl+R` — newid maint; `Ctrl+Shift+R` — maint y cynfas
* `Ctrl+[` / `Ctrl+]` — troi i'r chwith neu i'r dde
* `Ctrl+I` — gwrthdroi'r lliwiau
* `Ctrl+Shift+N` — haen newydd; `Ctrl+E` — uno i lawr; `Ctrl+Shift+E` — gwastatáu
* `Ctrl+Page Up` / `Ctrl+Page Down` — paentio ar yr haen uwchben neu islaw
* `Ctrl+Shift+Page Up` / `Ctrl+Shift+Page Down` — codi neu ostwng yr haen
* `S`, `P`, `B`, `A`, `E`, `C`, `F`, `D`, `I`, `T`, `L`, `R`, `O`, `Y`, `K`, `H`, `Z` — yr offer, yn eu trefn
* `Space` — wedi'i dal, llusgo'r llun gydag unrhyw offeryn
* `X` — cyfnewid y prif liw a'r ail liw
* `Tab` — trwy osodiadau'r offeryn, y stribed palet a'r doc lliw; `Shift+Tab` — yn ôl; `Escape` — yn ôl i'r llun
* `+` / `-` — chwyddo i mewn neu allan; `1` — maint gwirioneddol; `Ctrl+0` — ffitio
* `Ctrl` + olwyn — chwyddo i mewn neu allan o amgylch y pwyntydd
* Pinsio â dau fys — chwyddo'n llyfn; ar sgrin gyffwrdd mae'r llun yn dilyn y bysedd
* `G` — dangos neu guddio'r grid rhwng picseli
* `Ctrl+'` — dangos neu guddio'r grid
* `Page Up` / `Page Down` — y corlun neu'r dudalen cynt neu nesaf
* bysellau saeth — symud dewis sy'n arnofio un picsel; â `Shift`, deg; yn y stribed palet, camu trwy ei liwiau

## OPTIONS

`-h`, `-?`, `--help`
: Ysgrifennu'r cymorth hwn i'r allbwn safonol a gadael.

## EXIT STATUS

Sero ar ôl Gadael. Nid sero pan wrthodwyd sianel y ffenestr, blwch post y
digwyddiadau neu sesiwn y bwrdd gwaith; nodir y rheswm ar y ffrwd gwallau
safonol.
