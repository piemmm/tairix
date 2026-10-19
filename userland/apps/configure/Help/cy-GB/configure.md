## NAME

configure — darllen a gosod gosodiadau'r peiriant

## SYNOPSIS

`configure [<key> [<value> [<key> <value>]...]]`

## DESCRIPTION

Yn rhestru, dangos a gosod gosodiadau'r peiriant, a gedwir yn
`/System/Settings/Configuration/system.conf`. Heb operand rhestrir pob
gosodiad gyda'i werth cyfredol; gydag allwedd yn unig dangosir gwerth y
gosodiad hwnnw; gydag un neu ragor o barau `<key> <value>` newidir y
gosodiadau hynny gyda'i gilydd.

Mae sawl pâr yn un newid, nid yn sawl un: caiff pob pâr ei wirio cyn
newid dim, ac yna caiff y gosodiadau i gyd eu cadw gyda'i gilydd. Nid yw
llinell orchymyn sy'n enwi allwedd anhysbys, gwerth y tu allan i set ei
allwedd, neu'r un allwedd ddwywaith yn newid dim o gwbl, felly ni ellir
byth gadael grŵp o osodiadau wedi'i gymhwyso ar ei hanner.

Mae angen gweinyddwr i newid gosodiad: mae cyfrif cyffredin yn darllen
gosodiadau'r peiriant ond nid yw'n newid yr un ohonynt. Daw'r system â'i
gosodiadau ei hun, sy'n sefyll nes i weinyddwr newid un, ac mae'r newid
cyntaf yn cadw pob gosodiad arall a ddaeth gyda'r system. Daw newid i
rym y tro nesaf y bydd y rhan o'r system sy'n ei ddarllen yn cychwyn
(`os.loginType` yn y mewngofnodi nesaf, y switshis `cache.*` yn y
cychwyniad nesaf), oni bai fod ei allwedd isod yn dweud fel arall.

Mae'r set allweddi ar gau: gwrthodir allwedd anhysbys, neu werth y tu
allan i set allwedd, gan nodi'r dewisiadau dilys a heb newid dim.

- `os.loginType` — `text` neu `graphical`: pa fath o sesiwn y mae'r
  gwasanaeth mewngofnodi yn ei chychwyn i ddefnyddiwr wedi'i ddilysu.
  Mae `graphical` (y rhagosodiad) yn cychwyn y sesiwn bwrdd gwaith yn
  uniongyrchol ar ôl dilysu, gan ddisgyn yn ôl i'r mewngofnodi testun ar
  beiriant na all redeg un; mae `text` yn cychwyn cragen y cyfrif — dim
  ond o fewngofnodi graffigol y mae'r bwrdd gwaith yn rhedeg.
- `cache.all` — `on` neu `off`: y prif switsh storio dros dro. Mae
  `on` (y rhagosodiad) yn gadael i bob dosbarth storfa isod ddilyn ei
  osodiad ei hun; mae `off` yn nenfwd sy'n analluogi pob storfa gof
  waeth beth fo'r gosodiadau fesul dosbarth.
- `cache.filesystem`, `cache.block`, `cache.transform`,
  `cache.semantic` — `auto` neu `off`: y switshis fesul dosbarth ar
  gyfer y pedair storfa gof adenilladwy (storfeydd y system ffeiliau,
  y blociau disg cyfan, y clystyrau wedi'u datgywasgu, a chychwyn
  cymwysiadau). Mae `auto` (y rhagosodiad) yn gadael i reolwr pwysau'r
  cof lywodraethu'r dosbarth; mae `off` yn ei analluogi'n llwyr. Nid
  oes `on` fesul dosbarth: ni ellir gorfodi dosbarth i anwybyddu pwysau
  cof. Mae dosbarth yn effeithiol `off` pryd bynnag y bo `cache.all`
  yn `off`.

Mae pob storfa'n gyflymydd adenilladwy, byth yn ffynhonnell y gwir,
felly nid yw diffodd unrhyw un neu bob un ohonynt ond yn gwneud y gwaith
dan sylw yn arafach — nid yw byth yn newid canlyniad.

- `net.ipv4.enabled`, `net.ipv6.enabled` — `true` neu `false`:
  switshis teuluoedd cyfeiriadau ar draws y pentwr. Mae'r ddau yn
  `true` yn ddiofyn. Nid yw teulu analluogedig yn rhwymo cyfeiriadau,
  nid yw'n ateb unrhyw becyn, ac mae'n gwrthod soced o'r teulu hwnnw
  â gwall wedi'i deipio — byth yn ollwng distaw.
- `net.ipv6.privacy` — `true` neu `false`: a yw'r pentwr yn ffurfio
  cyfeiriadau IPv6 dros dro (preifatrwydd) yn ychwanegol at yr un
  sefydlog. Mae `false` (y diofyn) yn defnyddio'r cyfeiriad SLAAC
  sefydlog yn unig.
- `net.tcp.syncookies` — `auto` neu `always`: y polisi amddiffyn rhag
  llifogydd SYN. Mae `auto` (y diofyn) yn cadw ciw hanner-agored
  ffiniedig ac yn cwympo'n ôl at gwcis di-wladwriaeth pan fo'n
  gorlifo; mae `always` yn ateb pob cais cysylltu heb wladwriaeth.
  Nid oes `off` — nid yw ciw cysylltiadau diamddiffyn yn osodiad.
- `net.tcp.keepalive` — `true` neu `false`: a yw cysylltiadau TCP yn
  anfon archwiliadau cadw'n-fyw ar gyswllt segur. Nid yw `false` (y
  diofyn) byth yn archwilio nac yn gollwng cysylltiad segur; mae `true`
  yn archwilio cymar segur ar ôl y cyfnod arferol ac yn gollwng y
  cysylltiad os yw'n peidio ag ateb.
- `net.tcp.ecn` — `true` neu `false`: a yw cysylltiadau TCP yn negodi
  Hysbysiad Tagfa Eglur (ECN). Mae `false` (y diofyn) yn gadael
  cysylltiadau'n Not-ECT; mae `true` yn cynnig ECN yn yr ysgwyd llaw ac
  yna'n trin marc tagfa fel arwydd i arafu yn hytrach na gorfodi colli
  pecyn.
- `net.sockets.mem` — `auto` neu faint mewn beitiau fel `64M`: y cof y
  caiff y pentwr rhwydwaith ei ddal mewn cyflwr socedi ar draws pob
  endid. Mae `auto` (y diofyn) yn ei faintioli yn ôl RAM y peiriant, fel
  nad yw gweinydd mawr wedi'i rwymo i ffigwr a ddewiswyd ar un bach; mae
  maint yn disodli hynny ar gyfer llwyth rydych chi'n ei adnabod yn
  well. Caiff pob endid ddal un rhan o un deg chwech o'r gyllideb
  effeithiol, felly mae lle i un deg chwech bob amser. Beitiau yn
  hytrach na nifer o socedi, am fod yr un nifer o socedi yn ychydig
  gilobeitiau pan fyddant yn segur ac yn fegabeitiau pan fyddant yn
  llawn: mae'r gyllideb yn cario llawer o gysylltiadau tawel neu lai o
  rai prysur, fel y mae'r llwyth mewn gwirionedd.
- `time.servers` — `none` neu restr o weinyddion amser rhwydwaith wedi
  eu gwahanu â choma, pob un yn enw gwesteiwr neu'n gyfeiriad. Nid yw
  `none` (y diofyn) yn enwi gweinydd o'ch eiddo chi: caiff y cloc ei osod
  wedyn o'r gweinydd a enwir gan les cyfeiriad y rhwydwaith, neu fel arall
  o'r gronfa gyhoeddus `pool.ntp.org`. Mae gweinyddion a enwir yma yn drech
  na'r ddau.
- `time.refresh` — `6h`, `12h`, `1d`, `2d` neu `7d`: faint o amser rhedeg
  sy'n mynd heibio rhwng ymholiadau cloc unwaith y gwyddys yr amser. `1d`
  yw'r diofyn. Caiff cloc heb ei osod, cloc annhebygol, neu gloc hen iawn
  ei gywiro cyn gynted ag y bydd y rhwydwaith yn caniatáu, beth bynnag
  ddywed hyn.
- `time.network` — `on` (y diofyn) neu `off`: a gaiff y cloc ei osod o'r
  rhwydwaith o gwbl. Gydag `off` nid aiff ymholiad amser at unrhyw
  weinydd, a chaiff y cloc ei osod â llaw neu o sglodyn cloc y bwrdd, a
  ddarllenir ar bob cychwyn beth bynnag. Daw newid i rym ar unwaith; mae'r
  gosodiad ar ddisg y system, felly ar ddechrau cychwyn y diofyn sy'n
  gymwys nes cyrraedd y ddisg honno.
- `time.zone` — `default` neu enw cylchfa amser fel `Europe/London`: y
  gylchfa amser y mae'r peiriant yn ei chadw, sef yr un y mae'r sgrin
  fewngofnodi yn ei dangos ac y mae cyfrif nad yw wedi dewis un yn ei
  defnyddio. `default` yw'r gylchfa sy'n dod gyda'r system fel ei
  chylchfa ei hun; caiff enw nad yw yn rhestr cylchfaoedd y system ei
  hepgor fel pe bai'n `default`. Daw newid i rym ar unwaith.
- `input.mouse.debounce` — milieiliadau cyfan, `25` yn ddiofyn, `0` i'w
  ddiffodd, `100` ar y mwyaf: pa mor hir ar ôl rhyddhau botwm llygoden y
  caiff y gwasgiad nesaf o'r un botwm ei anwybyddu fel tincian switsh yn
  lle ei drin fel clic newydd. Gall switsh treuliedig adrodd ail wasgiad
  ychydig filieiliadau ar ôl ei ryddhau pan mai un clic a olygai. Rhowch `0`
  ar gyfer llygoden y mae ei modd tanio cyflym yn anfon parau o gliciau'n
  fwriadol.
- `audio.output`, `audio.input` — `auto` yn ddiofyn, neu leoliad pwynt
  terfyn fel y mae `audioctl` yn ei restru (un ar bymtheg o ddigidau hecs,
  dot, a mynegai'r pwynt terfyn): yr allbwn a'r mewnbwn y mae'r peiriant
  hwn yn eu ffafrio yn ddiofyn. Daw dewis sesiwn ei hun yn gyntaf; hwn yw'r
  dewis cyn i neb fewngofnodi ac ar beiriant heb fwrdd gwaith. `auto` yw'r
  ddyfais gyntaf a ganfuwyd.
- `audio.level` — mewn desibelau, `0dB` yn ddiofyn, neu wanhad fel `-12dB`
  neu `-6.5dB`, i ddau le degol ar y mwyaf: y lefel y mae pob allbwn a
  mewnbwn yn dechrau arni. Byth uwchben `0dB`. Mae'r gwasanaeth sain yn
  derbyn gwerth newydd yn y cychwyn nesaf; mae `audioctl` yn newid y
  peiriant sy'n rhedeg.

Darllenir allwedd nad yw yn y rhestr uchod yn erbyn ail gofrestr:
gosodiadau rhwydwaith pob rhyngwyneb, a gedwir yn
`/System/Settings/Network/network.conf`. Caiff ei hallweddi eu sillafu
`<interface>.<setting>`, lle mai `<interface>` yw'r enw arall a roddodd
gweinyddwr i ryngwyneb rhwydwaith (`wan`, `lan0`), ac mai `<setting>` yw
un o `kind`, `match.mac`, `match.node`, `ipv4.method`, `ipv4.address`,
`ipv4.gateway`, `ipv6.method`, `ipv6.address`, `ipv6.gateway`,
`dns.servers`, `mtu`, `bond.members`, `bond.mode`,
`bond.monitor-interval`, a `bond.primary`.

Chwilir cofrestr y peiriant uchod yn gyntaf bob tro, felly ni all enw
arall rhyngwyneb byth gymryd enw un o osodiadau'r peiriant drosodd.

Nid oes gan osodiadau'r rhwydwaith werthoedd diofyn, felly dim ond y
gosodiadau sydd wedi'u gosod a ddangosir wrth restru, ac mae dangos un
nad yw wedi'i osod yn ateb â llinell wag yn hytrach na dyfeisio gwerth.
Mae angen gweinyddwr i'w darllen neu eu newid: maent yn enwi caledwedd
pob rhyngwyneb a chyfeiriadau'r peiriant hwn. Mae rhestr unrhyw un arall
yn dangos gosodiadau'r peiriant ac yn dweud nad yw rhai'r rhwydwaith yn
cael eu dangos.

Gan nad oes gan y gofrestr honno werthoedd diofyn, caiff gosodiad ei
dynnu yn hytrach na'i ailosod, a **gwerth gwag** yw'r ffordd i dynnu un:
`configure wan.ipv4.address ""`. Nid yw'r un gosodiad rhyngwyneb yn
derbyn gwerth gwag, felly ni ellir byth camgymryd y sillafiad am osod
gwerth. Dyma sy'n gadael i un gorchymyn symud rhyngwyneb o gyfeiriad
sefydlog i DHCP —
`configure wan.ipv4.method dhcp wan.ipv4.address "" wan.ipv4.gateway ""`
— newid nad yw'r naill hanner na'r llall ohono'n disgrifio ar ei ben ei
hun gyfluniad sy'n dal at ei gilydd. Nid yw rhyngwyneb y tynnir ei
osodiad olaf wedi'i ddatgan o gwbl mwyach.

Caiff y cyfluniad cyfan a olygwyd ei wirio cyn cadw dim: gwrthodir newid
a fyddai'n ei adael yn anghyson (dull sefydlog heb gyfeiriad, bond â
llai na dau aelod) ac ni chedwir dim.

Mae newid gosodiad `net.*`, neu un rhyngwyneb, yn ei gadw ac yn ei
gyflwyno i'r pentwr rhwydwaith sy'n rhedeg, felly daw i rym ar unwaith.
Dim ond y rhyngwynebau yr effeithiodd y newid arnynt mewn gwirionedd a
gyflwynir. Os nad yw'r pentwr sy'n rhedeg yn derbyn cyflwyniad — nid oes
un yn rhedeg, neu ni chaiff eich cyfrif weinyddu'r rhwydwaith — caiff y
gosodiad ei gadw beth bynnag ac mae `configure` yn dweud hynny; yna daw
i rym yn y cychwyniad nesaf.

Adroddir dau gyfyngiad yn hytrach na'u cuddio. Mae rhyngwyneb a dynnoch
o'r cyfluniad yn dal i redeg tan y cychwyniad nesaf gyda'r cyfluniad a
roddwyd i'r pentwr ddiwethaf, am nad oes gan y pentwr neges sy'n dirwyn
un i ben. Ni ellir byth rhwymo rhyngwyneb a gadwyd heb `match.mac` na
`match.node` wrth ddyfais, felly caiff ei gadw a nodir y gwrthodiad.

## OPTIONS

- `-h, -?` — dangos cymorth byr y gorchymyn hwn.

## EXAMPLES

- `configure` — rhestru pob gosodiad.
- `configure os.loginType` — dangos y math sesiwn rhagosodedig.
- `configure os.loginType graphical` — cychwyn i'r mewngofnodi
  graffigol.
- `configure cache.all off` — analluogi pob storfa gof ar draws y
  system gyfan.
- `configure cache.filesystem off` — analluogi storfa'r system
  ffeiliau yn unig.
- `configure net.ipv6.enabled false` — diffodd IPv6 ar draws y pentwr.
- `configure time.servers 0.example.test,1.example.test` — gosod y
  gweinyddion amser rhwydwaith y caiff y cloc ei gydamseru â nhw.
- `configure time.zone Europe/London` — cadw'r peiriant ar amser
  Llundain.
- `configure wan.ipv4.address` — dangos y cyfeiriad IPv4 sefydlog a
  osodwyd ar gyfer y rhyngwyneb o'r enw `wan`.
- `configure wan.ipv4.address 10.0.0.7/24 wan.ipv4.gateway 10.0.0.1` —
  rhoi cyfeiriad IPv4 sefydlog a phorth rhagosodedig i `wan`.
- `configure wan.ipv4.method dhcp wan.ipv4.address "" wan.ipv4.gateway ""`
  — symud `wan` o gyfeirio sefydlog i DHCP.
- `configure wan.dns.servers 9.9.9.9,2001:db8::53` — gosod y gweinyddion
  DNS ailadroddol i'w defnyddio ar `wan`.

## EXIT STATUS

- `0` — cwblhawyd y rhestr, y gwerth, y cymorth byr neu'r newid.
- `1` — ni ellid darllen na newid y gosodiadau (er enghraifft ni chaiff
  y galwr newid gosodiadau'r peiriant, neu ni chaiff ddarllen ei
  osodiadau rhwydwaith), neu ni ellid cyflwyno'r allbwn.
- `2` — ni ddeallwyd y llinell orchymyn, mae'r allwedd yn anhysbys,
  mae'r gwerth y tu allan i set yr allwedd, neu byddai'r newid yn gadael
  cyfluniad y rhwydwaith yn anghyson.

## ENVIRONMENT

- `LANG` — hoff iaith y cymorth byr (tag BCP-47 fel `fr-FR`).

## SEE ALSO

- `man`
