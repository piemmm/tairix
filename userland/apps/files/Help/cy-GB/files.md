## NAME

files — porwr graffigol y system ffeiliau

## SYNOPSIS

`files [--desktop] [path] [-h | -?]`

## DESCRIPTION

Yn agor ffenestr bwrdd gwaith sy'n rhestru'r system ffeiliau, gan ddechrau
yn y ffolder y mae `path` yn ei henwi ar y llinell orchymyn — neu, pan
fo'n enwi ffeil, yn y ffolder sy'n dal y ffeil honno, gyda'r ffeil wedi'i
dewis — neu, os nad enwir un, yn y ffolder y mae'r gosodiadau'n ei dewis:
ffolder `Home` y defnyddiwr sy'n ei lansio, oni bai ei fod yn dewis ei
ffolder `Desktop` neu'r cyfrolau sydd wedi'u cysylltu (ei gyfeiriadur
cartref os na ellir rhestru hwnnw). Mae teitl y ffenestr yn enwi'r
cyfeiriadur cyfredol; mae'r ffenestr yn rhestru ei gofnodion, a phob cofnod
a ddewiswyd wedi'i amlygu â lliw acen y thema weithredol. Rhestriad
cyffredin wedi'i wirio yn erbyn caniatâd, dan hunaniaeth y defnyddiwr sy'n
ei lansio, yw pob darlleniad cyfeiriadur: gwrthodir cyfeiriadur na ellir ei
ddarllen, ac ni ddyfelir byth.

Mae'r bwrdd gwaith yn cychwyn y porwr i chi ac yn ei gadw ar y bar eiconau:
mae dewislen ei slot yn rhestru eich lleoedd eich hun a phopeth sydd wedi'i
osod, ac mae dewis un yn agor ffenestr yno; daw *Gosodiadau…* yn olaf. Mae
clic ar y slot yn agor un yn y man y mae'r gosodiadau'n ei ddewis. Mae gofyn
am ffolder sydd eisoes â ffenestr yn dod â'r ffenestr honno i'r blaen yn
hytrach nag agor un arall. Nid oes rhes *Gadael* gan y copi hwnnw — mae'n
rhan o'r bwrdd gwaith, ac mae cau ei ffenestri yn ei gadw o'r neilltu.

O'i redeg wrth ei enw o gragen (neu o'i agor ar ffolder o'r bwrdd gwaith)
mae'n rhaglen gyffredin yn lle hynny: un ffenestr, ac mae'n dod i ben pan
fyddwch yn ei chau. Y naill ffordd neu'r llall, mae angen sesiwn graffigol
sy'n rhedeg: hebddi, ni ellir cyrraedd sianel y ffenestri, ac mae'r porwr yn
adrodd y gwrthodiad ar y ffrwd gwallau safonol ac yn gadael.

Caiff y ffenestr ei gyrru â'r bysellfwrdd: mae `I lawr` ac `I fyny` yn symud
y dewisiad, mae `Enter` yn agor y cyfeiriadur a ddewiswyd, ac mae
`Backspace` yn mynd i fyny i'r cyfeiriadur rhiant. Mae `F5` yn ail-ddarllen
y rhestriad a cholofn y lleoedd; mae cyfrol newydd ei chysylltu yn ymddangos
yn y golofn ohoni ei hun. Mae `Ctrl+Shift+N` yn creu ffolder newydd.

Mae rhestriad yn agor heb ddim wedi'i ddewis. Mae clic yn dewis eitem, mae
clic gyda `Ctrl` yn ychwanegu neu'n tynnu un, ac mae clic gyda `Shift` yn
dewis y rhediad o'r eitem a ddewiswyd ddiwethaf; mae clic ar le gwag yn
clirio'r dewisiad. Mae clic dwbl yn agor eitem; o ddal `Shift`, mae'n
rhestru cynnwys rhaglen yn hytrach na'i chychwyn, ac yn gadael unrhyw beth
y mae'n ei agor mewn man arall heb ei ddewis. Mae llusgo ar draws lle gwag
yn tynnu blwch sy'n dewis popeth y mae'n ei gyffwrdd wrth iddo dyfu; o'i
ddal ar frig neu waelod y rhestriad, mae'n sgrolio, ac mae `Escape` yn
dadwneud yr hyn a ddewisodd.

Mae'r lle o amgylch pob eicon a'i enw'n cyfrif fel lle gwag, felly gall
blwch ddechrau unrhyw le rhwng eitemau. Mae `Ctrl+A` yn dewis popeth yn y
rhestriad ac mae `Ctrl+Shift+A` yn clirio'r dewisiad; mae'r ddau ar y
ddewislen clic de hefyd.

Dangosir enw eitem yn gyfan, dros ddwy linell pan fo angen; mae enw sy'n rhy
hir hyd yn oed i'r rheini'n cadw ei ddechrau a'i ddiwedd, gyda `…` yn y
canol, fel bod ei estyniad yn dangos bob amser. Dangosir rhaglen heb ei
hôl-ddodiad `.app`, a gall y gosodiadau ei ddangos, neu guddio ôl-ddodiaid y
mathau o ffeil y mae'r system yn eu hadnabod hefyd. Dim ond pan fo'r
gosodiadau'n gofyn amdanynt y rhestrir eitemau y mae eu henwau'n dechrau â
dot.

Mae `F2` yn ailenwi'r eitem a ddewiswyd yn ei lle, fel y mae clicio ar enw'r
unig eitem a ddewiswyd ac aros: mae'r enw'n agor i'w olygu cyn gynted ag na
all y clic fod yn glic dwbl mwyach. Dewisir y rhan cyn yr estyniad, felly
mae teipio'n disodli'r enw ac yn cadw'r estyniad; cedwir ôl-ddodiad nad yw'r
ffenestr yn ei ddangos hefyd. Mae `Enter`, neu glic y tu allan i'r enw, yn
cadw'r enw newydd — mae enw y mae'r gyfrol yn ei wrthod yn aros ar agor
gyda'r rheswm — ac mae `Escape` yn ei adael. Nid yw clic a ailenwodd yr
eitem yn gwneud dim byd arall, gan y gallai'r ffolder fod wedi'i haildrefnu
o dan y pwyntydd.

Mae llusgo eitemau a ddewiswyd ar ffenestr rheolwr ffeiliau arall, ar
ffolder ynddi, neu ar y bwrdd gwaith yn eu copïo yno; o ddal `Shift`, cânt
eu symud yn lle hynny. Mae'r pwyntydd yn dangos plws tra byddai gollwng yn
copïo a saeth tra byddai'n symud, ac mae'r ffolder y byddai gollwng yn
glanio ynddi wedi'i hamlygu. Caiff un ffeil a lusgir ar slot rhaglen ar y
bar eiconau ei hagor yno.

Mae'r ddewislen clic de yn dechrau gyda *Dangos*. Oddi tani mae
is-ddewislen yn enwi'r hyn y mae'r ddewislen yn gweithredu arno — *Ffeil
‘notes.txt’*, gydag enw hir wedi'i fyrhau yn ei ganol fel bod ei estyniad yn
dal i'w weld, neu *Dewis* pan fo sawl eitem wedi'u dewis — ac mae'n dal
*Agor*, *Agor gyda…*, *Cael gwybodaeth…*, *Ailenwi*, *Torri*, *Copïo* a
*Dileu*; mae rhes na all weithredu yno wedi'i llwydo ac yn dweud pam. Wedyn
daw *Newydd*, yna *Dewis y cyfan* a *Clirio'r dewis*, ac yn olaf *Gludo*. Mae
clic de ar le gwag yn clirio'r dewis, felly mae is-ddewislen yr eitem wedi'i
llwydo.

Mae is-ddewislen *Newydd* y ddewislen clic de yn creu ffolder, neu ddogfen
wag o bob math y mae golygydd wedi'i osod yn ei ysgrifennu, ac yn agor ei
henw i'w olygu.

Mae is-ddewislen *Dangos* y ddewislen clic de yn dangos yr eitemau fel
eiconau mawr, y rhagosodiad; fel eiconau bach, pob enw ar un llinell wrth
ymyl ei lun; neu fel rhestr, un rhes ar gyfer pob eitem gyda'i llun, ei
maint, pryd y cafodd ei newid ddiwethaf a'i math. Mae gyriant a restrir yn
eu plith yn dangos yn hytrach y lle sy'n rhydd arno, ei system ffeiliau a
pha mor dda y mae'n gwasanaethu. O dan y dulliau dangos, mae'n trefnu'r
ffenestr yn ôl enw, math, maint neu pryd y newidiwyd pob eitem ddiwethaf —
neu, ar gyfer rhestr o yriannau, yn ôl enw, lle rhydd, system ffeiliau neu
iechyd — yn esgynnol neu'n ddisgynnol. Mae clicio ar deitl colofn yn
trefnu'r rhestr yn ôl honno hefyd, ac mae ail glic yn gwrthdroi'r drefn. Mae
ffolderi'n aros ar y brig pa drefn bynnag a ddewisir, oni bai bod y
gosodiadau'n eu gosod ymhlith y ffeiliau.

Mae *Gosodiadau…* ar ddewislen slot y bar eiconau, neu `Ctrl+,`, yn agor
gosodiadau'r porwr: ble mae ffenestr newydd yn agor, sut mae'n dangos ac yn
trefnu ei heitemau, ac a yw'n dangos colofn y lleoedd a'r bar offer; a yw
ffolderi'n aros ar y brig; pa ôl-ddodiaid enwau a pha eitemau a guddir; a yw
llun yn dangos ei gynnwys ei hun a ffolder yr hyn sydd ynddi; ac a yw symud
eitemau i'r Sbwriel yn gofyn yn gyntaf. Mae *Adfer rhagosodiadau* yn rhoi
pob gosodiad yn ôl. Mae newid yn berthnasol i bob ffenestr ar unwaith ac yn
cael ei gofio; mae ffenestr sydd eisoes ar agor yn cadw'r dull dangos a'r
drefn sydd ganddi.

Mae `Alt+Enter` yn agor ffenestr *Gwybodaeth* yr eitem a ddewiswyd, fel y
mae *Cael gwybodaeth…* yn ei his-ddewislen yn y ddewislen clic de. Ffenestr ar ei phen ei hun yw
hi, felly gall sawl un fod ar agor ar unwaith ac mae'r rhestriad yn parhau'n
ddefnyddiadwy tra byddant: mae'n dangos beth yw'r eitem, ei maint, ei
stampiau amser, i ble mae alias yn pwyntio, ei chaniatâd a'i pherchennog,
a'r priodoleddau estynedig y mae'r gyfrol yn eu cadw ar ei chyfer. Gellir
newid caniatâd, perchennog a phriodoleddau yno, pob un fel ysgrifen
gyffredin wedi'i gwirio yn erbyn caniatâd dan eich hunaniaeth eich hun — mae
gwrthodiad yn dweud pam ac yn newid dim. Mae ailbennu perchennog yn gofyn am
y gallu `CAP_FS_CHOWN`; mae sesiwn hebddo yn gweld y perchennog a'r grŵp
wedi'u marcio â chlo, a llinell sy'n dweud pam.

Mae `Chwith` a `De` yn symud rhwng adrannau'r ffenestr. Ar *Caniatâd*, mae
`I lawr` neu `Tab` yn symud i mewn i'w rheolyddion: mae'r bysellau saeth yn
symud rhyngddynt, mae `Space` yn toglo caniatâd neu'n agor y perchennog
neu'r grŵp i'w olygu, ac mae `Tab` neu `Escape` yn dychwelyd at yr adrannau.

Trinnir yr operand `path` fel mewnbwn nad ymddiredir ynddo: rhaid iddo
fod yn llwybr absoliwt o fewn terfyn hyd llwybr y system, a rhaid i bob
un o'i gydrannau fod yn enw go iawn — nid yw `.` a `..` yn rhai felly,
fel na all sillafiad byth olygu rhywle heblaw'r hyn y mae i'w ddarllen.
Gwrthodir llwybr sy'n torri unrhyw un o'r rheolau hynny, neu na all y
defnyddiwr a'i lansiodd ei restru, gyda'r rheswm ar y ffrwd gwall
safonol, ac yna mae'r ffenestr yn agor yn y ffolder `Home` yn lle hynny,
fel nad yw ymresymiad gwael byth yn gadael y defnyddiwr heb ffenestr.
Gwrthodir ail operand yn llwyr yn hytrach na'i anwybyddu.

## OPTIONS

- `--desktop` — rhedeg fel cydran rheolwr ffeiliau'r bwrdd gwaith ei hun:
  slot parhaol ar y bar eiconau sy'n cynnig eich lleoedd a'r cyfrolau sydd
  wedi'u mowntio, dim ffenestr hyd nes y gofynnir am un, a dim ffordd i
  adael. Mae sesiwn y bwrdd gwaith yn pasio hyn wrth gychwyn; mae enwi
  `path` gydag ef yn cael ei wrthod, am nad yw cydran yn agor ffenestr
  i'w roi ynddi.
- `-h, -?` — dangos cymorth byr y gorchymyn hwn ei hun a gadael.

## EXIT STATUS

Sero ar ôl cau glân, neu ar ôl dangos y cymorth byr; `2` pan na
ddeallwyd y llinell orchymyn; fel arall heb fod yn sero pan wrthodwyd y
sianel ffenestr, y rhanbarth fframiau a rennir, neu restriad cychwynnol
y cyfeiriadur (nodir y rheswm ar y ffrwd gwall safonol).
