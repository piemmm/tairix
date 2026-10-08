## NAME

files — porwr graffigol y system ffeiliau

## SYNOPSIS

`files [--desktop] [directory] [-h | -?]`

## DESCRIPTION

Yn agor ffenestr bwrdd gwaith sy'n rhestru'r system ffeiliau, gan ddechrau
yn y `directory` a enwir ar y llinell orchymyn, neu, os nad enwir un, yn
ffolder `UserFiles` y defnyddiwr sy'n ei lansio (ei gyfeiriadur cartref os
na ellir rhestru hwnnw). Mae teitl y ffenestr yn enwi'r cyfeiriadur
cyfredol; mae'r ffenestr yn rhestru ei gofnodion, a phob cofnod a ddewiswyd
wedi'i amlygu â lliw acen y thema weithredol. Rhestriad cyffredin wedi'i
wirio yn erbyn caniatâd, dan hunaniaeth y defnyddiwr sy'n ei lansio, yw pob
darlleniad cyfeiriadur: gwrthodir cyfeiriadur na ellir ei ddarllen, a ni
ddyfelir byth.

Mae'r bwrdd gwaith yn cychwyn y porwr i chi ac yn ei gadw ar y bar eiconau:
mae dewislen ei slot yn rhestru eich lleoedd eich hun a phopeth sydd wedi'i
osod, ac mae dewis un yn agor ffenestr yno. Mae clic ar y slot yn agor un yn
eich ffolder `UserFiles`. Mae gofyn am ffolder sydd eisoes â ffenestr yn dod
â'r ffenestr honno i'r blaen yn hytrach nag agor un arall. Nid oes rhes
*Gadael* gan y copi hwnnw — mae'n rhan o'r bwrdd gwaith, ac mae cau ei
ffenestri yn ei gadw o'r neilltu.

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
clirio'r dewisiad. Mae llusgo ar draws lle gwag yn tynnu blwch sy'n dewis
popeth y mae'n ei gyffwrdd wrth iddo dyfu; o'i ddal ar frig neu waelod y
rhestriad, mae'n sgrolio, ac mae `Escape` yn dadwneud yr hyn a ddewisodd.

Mae llusgo eitemau a ddewiswyd ar ffenestr rheolwr ffeiliau arall, ar
ffolder ynddi, neu ar y bwrdd gwaith yn eu copïo yno; o ddal `Shift`, cânt
eu symud yn lle hynny. Mae'r pwyntydd yn dangos plws tra byddai gollwng yn
copïo a saeth tra byddai'n symud, ac mae'r ffolder y byddai gollwng yn
glanio ynddi wedi'i hamlygu. Caiff un ffeil a lusgir ar slot rhaglen ar y
bar eiconau ei hagor yno.

Mae is-ddewislen *Newydd* y ddewislen clic de yn creu ffolder, neu ddogfen
wag o bob math y mae golygydd wedi'i osod yn ei ysgrifennu, ac yn agor ei
henw i'w olygu.

Mae `Alt+Enter` yn agor ffenestr *Priodweddau* ar yr eitem a ddewiswyd, fel
y mae rhes *Priodweddau* y ddewislen clic de. Ffenestr ar ei phen ei hun yw
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

Trinnir yr operand `directory` fel mewnbwn nad ymddiredir ynddo: rhaid
iddo fod yn llwybr absoliwt o fewn terfyn hyd llwybr y system, a rhaid
i bob un o'i gydrannau fod yn enw cyfeiriadur go iawn — nid yw `.` a
`..` yn rhai felly, fel na all sillafiad byth olygu rhywle heblaw'r hyn
y mae i'w ddarllen. Gwrthodir cyfeiriadur sy'n torri unrhyw un o'r
rheolau hynny, neu na all y defnyddiwr a'i lansiodd ei restru, gyda'r
rheswm ar y ffrwd gwall safonol, ac yna mae'r ffenestr yn agor yn y
ffolder `UserFiles` yn lle hynny, fel nad yw ymresymiad gwael byth yn
gadael y defnyddiwr heb ffenestr. Gwrthodir ail operand yn llwyr yn
hytrach na'i anwybyddu.

## OPTIONS

- `--desktop` — rhedeg fel cydran rheolwr ffeiliau'r bwrdd gwaith ei hun:
  slot parhaol ar y bar eiconau sy'n cynnig eich lleoedd a'r cyfrolau sydd
  wedi'u mowntio, dim ffenestr hyd nes y gofynnir am un, a dim ffordd i
  adael. Mae sesiwn y bwrdd gwaith yn pasio hyn wrth gychwyn; mae enwi
  `directory` gydag ef yn cael ei wrthod, am nad yw cydran yn agor ffenestr
  i'w roi ynddi.
- `-h, -?` — dangos cymorth byr y gorchymyn hwn ei hun a gadael.

## EXIT STATUS

Sero ar ôl cau glân, neu ar ôl dangos y cymorth byr; `2` pan na
ddeallwyd y llinell orchymyn; fel arall heb fod yn sero pan wrthodwyd y
sianel ffenestr, y rhanbarth fframiau a rennir, neu restriad cychwynnol
y cyfeiriadur (nodir y rheswm ar y ffrwd gwall safonol).
