## NAME

files — porwr graffigol y system ffeiliau

## SYNOPSIS

`files --desktop [--mount] [path] [-h | -?]`

## DESCRIPTION

Rheolwr ffeiliau'r bwrdd gwaith ei hun, sy'n rhestru'r system ffeiliau mewn
ffenestri. Mae ffenestr yn agor yn y ffolder y gofynnir amdani — neu, yn
achos ffeil, yn y ffolder sy'n dal y ffeil honno, gyda'r ffeil wedi'i
dewis — neu, os nad enwir un, yn y ffolder y mae'r gosodiadau'n ei dewis:
ffolder `Home` y defnyddiwr sy'n ei lansio, oni bai ei fod yn dewis ei
ffolder `Desktop` neu'r cyfrolau sydd wedi'u cysylltu (ei gyfeiriadur
cartref os na ellir rhestru hwnnw). Mae teitl y ffenestr yn enwi'r
cyfeiriadur cyfredol; mae'r ffenestr yn rhestru ei gofnodion, a phob cofnod
a ddewiswyd wedi'i amlygu â lliw acen y thema weithredol. Rhestriad
cyffredin wedi'i wirio yn erbyn caniatâd, dan hunaniaeth y defnyddiwr sy'n
ei lansio, yw pob darlleniad cyfeiriadur: gwrthodir cyfeiriadur na ellir ei
ddarllen, ac ni ddyfelir byth.

Mae'r bwrdd gwaith yn cychwyn y rheolwr ffeiliau gyda'r sesiwn ac yn ei gadw
ar y bar eiconau, ac nid yw byth yn gadael: mae dewislen ei slot yn rhestru
eich lleoedd eich hun a phopeth sydd wedi'i osod, ac mae dewis un yn agor
ffenestr yno; daw *Rhwydwaith* a *Gosodiadau…* yn olaf. Mae cyfrol y gallwch
ei thaflu allan — cof bach USB, cerdyn cof neu ddisg symudadwy arall, neu
ddelwedd ddisg neu gyfran a osodwyd gennych — yn agor is-ddewislen sy'n dal
*Taflu allan*, ac mae delwedd ddisg o'ch eiddo sydd â theitl yn dal *Teitl*
hefyd: mae'n agor maes sy'n dal teitl y ddisg, yn barod i'w newid, ac mae
pwyso `Return` yn ysgrifennu'r un newydd i'r ddisg. Mae clic ar y slot yn
agor un yn y man y mae'r gosodiadau'n ei ddewis. Mae gofyn am ffolder sydd
eisoes â ffenestr yn dod â'r ffenestr honno i'r blaen yn hytrach nag agor un
arall. Nid oes rhes *Gadael* ar y ddewislen — mae'r rheolwr ffeiliau'n rhan
o'r bwrdd gwaith, mae cau ei ffenestri yn ei gadw o'r neilltu, ac os daw i
ben rywbryd, mae'r bwrdd gwaith yn ei gychwyn eto.

Mae *Rhwydwaith*, ar ddewislen y slot yn union uwchben *Gosodiadau…*, yn
agor ffenestr ar y cyfrifiaduron ar y rhwydwaith lleol sy'n rhannu ffeiliau.
Rhestrir pob un gyda'i enw, y cyfeiriad lle mae ar gael, ei weithgor neu ei
barth, a'r hyn ydyw, ac mae'r rhestr yn llenwi wrth i gyfrifiaduron ateb.
Dim ond tra bo ffenestr yn ei dangos y caiff y rhwydwaith ei chwilio. Mae
cyfrifiadur sydd wedi peidio ag ateb yn llwyd nes iddo gael ei anghofio; mae
enw sy'n cymysgu gwyddorau yn y ffordd y mae dynwared enw arall yn gofyn
amdani yn dangos arwydd rhybudd yn lle ei lun; a phan fo un cyfrifiadur yn
cynnig mwy nag ychydig o gofnodion, caiff y gweddill eu cyfrif mewn rhes eu
hunain yn hytrach na gwthio'r lleill o'r neilltu. Pwy bynnag sy'n rhedeg pob
cyfrifiadur sy'n dewis ei enw ar y rhwydwaith, felly nid yw enw byth yn
profi pa gyfrifiadur rydych chi'n edrych arno. Nid yw cysylltu â chyfrannau
cyfrifiadur ar gael ar y system hon, ac mae agor un yn dweud hynny.

Mae'n rhedeg dim ond pan fo'r bwrdd gwaith yn ei gychwyn: o'i gychwyn mewn
unrhyw ffordd arall — o gragen wrth ei lwybr,
`/System/Components/files.app` — mae'n gwrthod rhedeg, gan nodi pam ar y
ffrwd gwallau safonol. Mae angen sesiwn graffigol sy'n rhedeg: hebddi, ni
ellir cyrraedd sianel y ffenestri, ac mae'n adrodd y gwrthodiad ar y ffrwd
gwallau safonol ac yn gadael.

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
Mae copïo neu symud yn cadw tyllau pob ffeil a'i phriodoleddau
estynedig, megis math ffeil RISC OS; caiff unrhyw briodoledd na all y
gyrchfan ei chadw ei henwi mewn rhybudd pan ddaw'r weithred i ben.

Mae'r ddewislen clic de yn dechrau gyda *Dangos*. Oddi tani mae is-ddewislen
yn enwi'r hyn y mae'r ddewislen yn gweithredu arno — *Ffeil ‘notes.txt’*,
gydag enw hir wedi'i fyrhau yn ei ganol fel bod ei estyniad yn dal i'w weld,
neu *Dewis* pan fo sawl eitem wedi'u dewis — ac mae'n dal *Agor*, *Agor
gyda…*, *Mowntio darllen-yn-unig*, *Taflu allan*, *Cael gwybodaeth…*,
*Ailenwi*, *Torri*, *Copïo* a *Dileu*; mae rhes na all weithredu yno wedi'i
llwydo ac yn dweud pam. Wedyn daw *Newydd*, yna *Dewis y cyfan* a *Clirio'r
dewis*, ac yn olaf *Gludo*. Mae clic de ar le gwag yn clirio'r dewis, felly
mae is-ddewislen yr eitem wedi'i llwydo.

Mae agor delwedd ddisg neu archif — ffeil `.iso`, `.adf`, `.hdf`, `.zip` ac
ati — yn ei gosod i chi yn `/Storage`, yn ysgrifenadwy lle mae'r ffeil a'i
fformat ill dau'n caniatáu hynny, ac yn agor ei ffenestr; mae
*Mowntio darllen-yn-unig* yn ei gosod yn ddarllen-yn-unig yn lle hynny.
Mae agor un ar y bwrdd gwaith yn gwneud yr un peth. Unwaith na fydd unrhyw
ffenestr yn ei dangos mwyach, caiff ei dad-osod eto, fel mai dim ond yr hyn
sy'n cael ei ddefnyddio sydd yn `/Storage`; os oes rhywbeth arall yn dal
i'w defnyddio — dogfen ohoni ar agor mewn rhaglen arall, dyweder — mae'n
aros wedi'i gosod, a rhoddir cynnig arall ar ei dad-osod unwaith y gallai
hwnnw fod wedi'i gollwng. Gall y gosodiadau gadw'r hyn rydych chi'n ei agor
wedi'i osod yn lle hynny, nes i chi ei daflu allan neu i'ch sesiwn
fewngofnodi ddod i ben. Mae *Taflu allan* yn
tynnu cyfrol a osodwyd gennych, neu ddisg symudadwy tra byddwch chi'n
defnyddio'r cyfrifiadur ei hun, o'i heicon, o'i rhes yn y bar lleoedd, y mae
ei dewislen clic de ei hun yn dal *Agor* a *Taflu allan*, neu o ddewislen
slot y bar eiconau; mae'ch ffenestri arni'n symud i `/Storage` yn gyntaf. Ni
chaiff disg symudadwy sydd â ffeil arni'n dal ar agor ei thaflu allan, ac
mae'n dweud hynny. Pan na chewch chi daflu disg symudadwy allan — nid chi
yw'r person wrth y cyfrifiadur — mae'r bwrdd gwaith yn gofyn am enw a
chyfrinair gweinyddwr, ac yn ei thaflu allan ar air y gweinyddwr. Mae cyfrol
a ddiflannodd —
ei disg wedi'i thynnu allan, neu ei gweinydd wedi'i golli — cyn i bopeth a
ysgrifennwyd gennych iddi gael ei gadw yn gadael y bar ond yn cadw ei heicon
yn `/Storage`, a chollir yr hyn na chafodd ei gadw; mae *Taflu allan* ar
yr eicon hwnnw'n gofyn cyn ei gollwng.

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
gosodiadau'r porwr yn y rhaglen Gosodiadau, dan *Ffeiliau*: ble mae ffenestr
newydd yn agor, sut mae ffenestri'n dangos ac yn trefnu eu heitemau, ac a
yw un newydd yn dangos colofn y lleoedd a'r bar offer; a yw ffolderi'n aros
ar y brig; pa ôl-ddodiaid enwau a pha eitemau a guddir; a yw llun yn dangos
ei gynnwys ei hun a ffolder yr hyn sydd ynddi; a yw symud eitemau i'r
Sbwriel yn gofyn yn gyntaf; ac a gaiff delwedd ddisg neu archif a agorwyd
gennych ei dad-osod unwaith y caiff ei ffenestri eu cau. Mae newid yn
berthnasol i bob ffenestr ar unwaith ac yn cael ei gofio; mae ffenestr yn
cadw dull dangos neu drefn a ddewisoch chi ynddi eich hun.

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

- `--desktop` — rhedeg fel rheolwr ffeiliau'r bwrdd gwaith ei hun: slot
  parhaol ar y bar eiconau sy'n cynnig eich lleoedd a'r cyfrolau sydd wedi'u
  mowntio, a dim ffordd i adael. Mae sesiwn y bwrdd gwaith yn pasio hyn bob
  tro y bydd yn cychwyn y rheolwr ffeiliau, nad yw'n rhedeg hebddo; mae
  `path` gydag ef yn agor ffenestr yno.
- `--mount` — agor `path` fel delwedd ddisg neu archif, yn union fel ei
  hagor mewn ffenestr: caiff ei gosod i chi ac mae ei ffenestr yn agor, a
  chaiff ei dad-osod eto unwaith y caiff ei ffenestri eu cau, fel y dywed y
  gosodiadau. Mae angen `path` arno.
- `-h, -?` — dangos cymorth byr y gorchymyn hwn ei hun a gadael.

## EXIT STATUS

Nid yw'n gadael tra bo'r bwrdd gwaith yn rhedeg. Sero ar ôl dangos y cymorth
byr; `2` pan na ddeallwyd y llinell orchymyn neu pan nad oedd yn enwi
`--desktop`; fel arall heb fod yn sero pan wrthodwyd y sianel ffenestr neu'r
rhanbarth fframiau a rennir (nodir y rheswm ar y ffrwd gwall safonol).
