## NAME

datetime — dyddiad, amser a chylchfa amser y peiriant

## SYNOPSIS

`datetime`

## DESCRIPTION

Yn agor y ffenestr **Date & Time** (Dyddiad ac Amser): yr hyn y mae cloc
y peiriant yn ei ddangos, o ble y daeth yr amser hwnnw, a'r rheolyddion
sy'n ei osod. Cyrhaeddir y ffenestr o ddewislen cloc y bwrdd gwaith ei
hun — de-gliciwch ar y cloc yn y bar eiconau a dewis **Set Date & Time…**
(Gosod Dyddiad ac Amser…) — a chyn iddi agor, mae'r bwrdd gwaith yn
gofyn am gyfrinair cyfrif sy'n cael newid y gosodiadau hyn.

Mae brig y ffenestr yn dangos yr amser i'r eiliad, y dyddiad, a'r
gylchfa amser gyda'i thalfyriad a'i gwrthbwyso o UTC, a'r cyfan yng
nghylchfa'r peiriant ei hun.

Mae **Date and Time** (Dyddiad ac Amser) yn dal y cloc ei hun.

- Mae **Set date and time automatically** (Gosod y dyddiad a'r amser yn
  awtomatig) yn cadw'r cloc yn gywir drwy weinyddion amser ar y
  rhwydwaith. Oddi tano mae'r ffenestr yn dweud o ble y daeth amser y
  cloc: wedi'i osod o'r rhwydwaith, a phryd; wedi'i osod â llaw; wedi'i
  ddarllen o sglodyn cloc y bwrdd pan gychwynnodd y peiriant; neu heb ei
  osod o gwbl. Daw troi'r switsh ymlaen neu i ffwrdd i rym ar unwaith.
- Tra bo'r switsh ymlaen, dangosir y calendr a'r amser ond ni ellir eu
  newid. Trowch ef i ffwrdd i osod y cloc â llaw.
- Dewiswch ddiwrnod yn y calendr a theipio'r amser neu ei newid fesul
  cam, yna pwyswch **Set**. Mae'r bysellau saeth yn symud y diwrnod a
  ddewiswyd fesul diwrnod a fesul wythnos, `Page Up` a `Page Down` fesul
  mis, a chyda `Shift` fesul blwyddyn; mae'r botymau wrth ymyl y mis a'r
  flwyddyn yn symud drwyddynt heb newid y diwrnod. Ym maes yr amser, mae
  `Tab` neu `:` yn symud o'r awr i'r munud ac i'r eiliad, ac mae `Up` a
  `Down` yn newid fesul cam y rhan y mae'r cyrchwr ynddi. Ar gloc deuddeg
  awr, **AM** neu **PM** yw'r rhan olaf: cliciwch arni, neu pwyswch `A`
  neu `P`. Mae `Enter` yn gosod y cloc.
- Hyd nes i chi ddewis diwrnod neu newid yr amser, mae'r calendr a'r
  amser yn dilyn y cloc wrth iddo redeg. Unwaith y gwnewch hynny, maen
  nhw'n dal yr hyn a ddewisoch hyd nes i chi osod y cloc neu gau'r
  ffenestr.

Darllenir y dyddiad a'r amser a osodwch yng nghylchfa amser y peiriant.
Gwrthodir amser y mae'r clociau'n ei hepgor pan fyddant yn mynd ymlaen,
ac mae'r ffenestr yn dweud pryd mae'r clociau'n newid. Am amser y mae'r
clociau'n mynd drwyddo ddwywaith pan fyddant yn mynd yn ôl, mae'r
ffenestr yn gofyn: dewiswch a ydych yn golygu'r un cyn y newid neu'r un
ar ei ôl. Ni chaiff dyddiad cyn i'r system hon gael ei gwneud, neu fwy
na chanrif ar ôl iddi gael ei gwneud, ei osod nes i chi ei gadarnhau,
oherwydd ni fyddai tystysgrifau'n cael eu derbyn fel rhai dibynadwy a
byddai ffeiliau'n edrych yn hŷn, neu'n fwy newydd, nag ydynt.

Ar ôl gosod y cloc, caiff sglodyn cloc y bwrdd ei osod i'r un amser, fel
bod y peiriant yn ei gadw pan fydd yn ailgychwyn. Os nad oes gan y
peiriant sglodyn cloc, mae'r ffenestr yn dweud y bydd yr amser yn cael ei
golli wrth ailgychwyn y tro nesaf oni bai bod amser rhwydwaith ymlaen.

Mae **Time Zone** (Cylchfa Amser) yn enwi'r gylchfa y mae'r peiriant yn
ei chadw, yn dweud pryd y bydd ei chlociau'n mynd ymlaen neu'n ôl nesaf
a chan faint, ac yn cynnig y botwm **Change…**, sy'n agor rhestr o bob
lle y mae'r system yn ei adnabod: teipiwch ran o enw dinas, rhanbarth
neu gylchfa i'w hidlo, yna dewiswch ag `Enter` neu â chlic dwbl. Mae pob
lle'n dangos yr amser yno ar hyn o bryd. Daw cylchfa newydd i rym ar
unwaith.

Cylchfa'r peiriant yw'r un y mae'r sgrin fewngofnodi'n ei dangos, a'r un
a ddefnyddir gan bob cyfrif nad yw wedi dewis ei gylchfa ei hun yn
**Settings** (Gosodiadau).

Os nad yw'r cyfrif y mae'r ffenestr yn rhedeg fel ef yn cael gwneud
newid, gwrthodir y newid, mae rhybudd yn dweud pam, ac mae'r ffenestr yn
dangos yr hyn sydd mewn grym mewn gwirionedd. Mae `Escape` neu **Close**
yn cau'r ffenestr.

## EXIT STATUS

Sero pan gaiff y ffenestr ei chau, gan gynnwys ar ôl newid a wrthodwyd.
Nid sero pan na allwyd agor y ffenestr neu pan gollwyd ei chysylltiad
â'r bwrdd gwaith; nodir y rheswm ar y ffrwd gwall safonol.

## SEE ALSO

`configure`
