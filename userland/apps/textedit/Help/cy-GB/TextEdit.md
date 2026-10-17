## NAME

TextEdit — golygydd testun a hecs graffigol

## SYNOPSIS

`TextEdit`

## DESCRIPTION

Yn golygu unrhyw ffeil mewn ffenestr bwrdd gwaith: testun, cod ffynhonnell,
ffeiliau gosodiadau'r system, neu feitiau crai. Wedi'i lansio â dogfen — o'r
rheolwr ffeiliau, o'r bwrdd gwaith, neu drwy ollwng ffeil ar ei eicon ar y
bar eiconau — mae'n agor ffenestr ar y ffeil honno. Wedi'i lansio ar ei ben
ei hun, mae'n agor ffenestr wag. Mae pob dogfen yn ffenestr o'r un golygydd;
mae cau'r olaf yn ei adael ar y bar eiconau, a'r rhes "Gadael" yn newislen
ei eicon sy'n ei derfynu.

Nid oes dim o gynnwys ffeil yn cael ei guddio. Dangosir beit rheoli fel
`[x03]`, beit nad yw'n UTF-8 dilys fel `[xC3]`, a nod anweledig neu nod sy'n
newid cyfeiriad yr ysgrifen fel `[U+202E]`, pob un yn ei liw ei hun a phob un
yn un cam i'r cyrchwr. Mae ffeil sy'n edrych fel data deuaidd yn agor yn yr
olwg hecs, sy'n dangos pob beit fel dau ddigid hecs wrth ymyl ei nod ac yn
golygu'r un beitiau â'r olwg testun.

Mae ffeil weithredadwy — delwedd rhaglen TAIRiX, ffeil ELF64 neu fodiwl
WebAssembly — yn agor yn y wedd cod yn lle hynny: ei rhanbarthau, yna
cyfarwyddiadau pob rhanbarth cod, un i bob llinell gyda'i gyfeiriad a'i
feitiau amgodio, wedi'u labelu â'r symbolau y maent yn eu cychwyn a'r
symbolau y mae eu canghennau'n eu cyrraedd. Darllen yn unig yw'r wedd cod;
mae'r gweddau testun a hecs yn golygu'r un ffeil. Mae ail broses waith ei hun,
heb unrhyw gyrhaeddiad chwaith, yn datgodio'r cod, a'i ddarllen fel y set
gyfarwyddiadau y mae'r ffeil yn ei henwi, neu fel set y peiriant hwn fel arall;
mae "Set gyfarwyddiadau" yn y ddewislen Gwedd yn ei ddarllen fel un arall, ac
mae maes gwedd y llinell statws yn cynnig y wedd cod wrth ochr testun a hecs.

Mae llun — PNG, JPEG, GIF, BMP, TIFF, WEBP, ffeil eicon neu bwyntydd, ardal
corluniau RISC OS neu ddogfen OpenRaster — yn agor yn hytrach yn y wedd
ddata: o beth mae'r ffeil wedi'i gwneud, pob cofnod yn ei safle gyda'i hyd
a'r hyn y mae'n ei ddatgan, pob tag EXIF wrth ei enw gyda'i werth, ac yna
pob beit o'r ffeil mewn hecs wrth ymyl ei nod. Mae'r wedd ddata yn ddarllen
yn unig; mae'r wedd hecs yn golygu'r un beitiau. Caiff y llun ei ddarllen
gan drydedd broses waith ei hun, heb gyrraedd dim chwaith, ac mae'r
ddewislen Gwedd yn cynnig y wedd ddata wrth ymyl testun a hecs.

Mae cod ffynhonnell yn cael ei liwio: HTML, XML ac SVG, CSS, JavaScript, JSON,
YAML, TOML, Markdown, Rust, C, Java, Python a sgriptiau cragen. Mae ffeiliau
gosodiadau'r system — gosodiadau rhaglenni, llyfrgell y rhaglenni,
ffurfweddiad y system a'r rhwydwaith, diystyriadau gwasanaethau, cronfeydd
data'r defnyddwyr a'r grwpiau, a maniffestau teuluoedd ffontiau — yn cael eu
lliwio hefyd ac yn cael eu gwirio â'r dosrannydd y mae'r system yn eu darllen
ag ef: nodir problem yn yr ymyl wrth ymyl ei llinell ac fe'i datgenir yn y
llinell statws. Dewisir y fformat o enw'r ffeil, ac yna o'i beitiau cyntaf;
mae fformat a ddewisir o'r ddewislen Golwg neu o'r llinell statws bob amser
yn drech.

Nid oes gan y golygydd unrhyw allu dros y system ffeiliau. Mae'n golygu dim
ond y ffeil a roddwyd iddo. Rhoddir ffeil y caiff y defnyddiwr ei newid yn
ysgrifenadwy, ac mae Cadw yn ei hysgrifennu'n ôl; mae unrhyw ffeil arall yn
ddarllen-yn-unig, ac mae Cadw yn gofyn ble i gadw copi. Mae'r lliwio, canfod
y fformat a'r gwirio yn rhedeg mewn proses waith ar wahân heb unrhyw
gyrhaeddiad o gwbl, felly ni all ffeil elyniaethus gyffwrdd â dim y gall y
golygydd ei gyrraedd.

Mae gwasgu ail fotwm (de) y llygoden unrhyw le yn y ffenestr yn agor ei
dewislen: Torri, Copïo, Gludo a Dewis y cyfan, ac yna Ffeil, Golygu, Canfod
a Golwg, pob un yn agor ei is-ddewislen ei hun. Nid oes bar dewislen gan y
ffenestr.

Mae'r llinell statws yn dangos llinell a cholofn y cyrchwr, yr hyn a ganfu'r
gwirio, ac, fel meysydd sy'n agor dewislen o'u clicio: y fformat, testun neu
hecs, diwedd y llinellau, a'r mewnoliad. Mae cau ffenestr neu adael â
newidiadau heb eu cadw yn gofyn yn gyntaf.

* `Ctrl+N` — ffenestr newydd
* `Ctrl+O` — agor ffeil
* `Ctrl+S` — cadw; `Ctrl+Shift+S` — cadw fel
* `Ctrl+W` — cau'r ffenestr
* `Ctrl+Z` — dadwneud; `Ctrl+Shift+Z` neu `Ctrl+Y` — ailwneud
* `Ctrl+X`, `Ctrl+C`, `Ctrl+V` — torri, copïo, gludo
* `Ctrl+A` — dewis popeth
* `Ctrl+F` — canfod; `Ctrl+H` — amnewid
* `F3` / `Shift+F3` — y cyfatebiaeth nesaf neu flaenorol
* `Ctrl+L` — mynd i linell
* `F8` — y broblem nesaf a ganfu'r gwirio
* `Ctrl+]` / `Ctrl+[` — mewnoli neu allanoli'r llinellau a ddewiswyd
* `Ctrl+/` — troi'r llinellau a ddewiswyd yn sylwadau neu'n ôl
* `Ctrl+Shift+H` — newid rhwng yr olwg testun a'r olwg hecs
* `Insert` — newid rhwng mewnosod a throsysgrifo
* `Tab` — yn yr olwg hecs, symud rhwng y colofnau hecs a nodau

## OPTIONS

`-h`, `-?`, `--help`
: Ysgrifennu'r cymorth hwn i'r allbwn safonol a therfynu.

## EXIT STATUS

Sero ar ôl Gadael. Nid sero pan wrthodwyd sianel y ffenestr, blwch post y
digwyddiadau, neu sesiwn y bwrdd gwaith; nodir y rheswm ar y llif gwallau
safonol.
