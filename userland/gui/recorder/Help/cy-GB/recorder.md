## NAME

recorder — recordio'r sgrin ar gyfer y bwrdd gwaith

## SYNOPSIS

`recorder SESSION`

## DESCRIPTION

Yn recordio'r sgrin yn ffilm tra bo'r bwrdd gwaith yn gofyn iddo. Nid yw'n
cael ei redeg â llaw: mae'r bwrdd gwaith yn ei gychwyn pan fydd recordiad yn
dechrau, ac mae'n dod i ben pan ddaw'r recordiad i ben.

Mae'r rhaglen sy'n ei gychwyn yn enwi fel `SESSION` sesiwn y bwrdd gwaith
y mae'n recordio ar ei chyfer.

I recordio, pwyswch `Meta+Shift+5` i agor y bar cipio, dewiswch y sgrin
gyfan, ffenestr neu ardal, a phwyswch **Record** (Recordio). Mae llwybr byr
cipio, y dangosydd recordio yn yr ardal hysbysiadau, neu'r hyd a ddewiswyd
yn **Options** (Dewisiadau) yn ei atal.

Caiff recordiad ei gadw fel `Screen Recording YYYY-MM-DD at HH.MM.SS.webm`
yn y ffolder Desktop neu yn `Home/Videos`, fel y mae **Options** yn ei
ddewis. Ffilm WebM ydyw: ei llun yn fideo VP8 hyd at dri deg ffrâm yr
eiliad, wedi'u cymryd dim ond pan fydd rhywbeth ar y sgrin yn newid, a'i
sain yn Opus.

Mae **Sound** (Sain) yn **Options** yn dewis yr hyn a glyw'r recordiad: dim
byd, y meicroffon, sain y bwrdd gwaith ei hun, neu'r ddau wedi'u cymysgu.
Mae recordiad gweinyddwr gyda sain yn dechrau ar unwaith. Mae recordiad
unrhyw gyfrif arall yn gofyn am gyfrinair gweinyddwr yn gyntaf, ac yna mae'r
recordydd yn rhedeg fel y gweinyddwr hwnnw ar gyfer yr un recordiad hwnnw;
nid yw canslo'n recordio dim, ac mae hysbysiad yn dweud hynny.

Pan fydd y ddisg yn llenwi, mae'r recordiad yn dod i ben yno ac yn cael ei
gadw. Pan fydd y recordydd yn methu, caiff yr hyn a recordiodd ei gadw ac
mae hysbysiad yn dweud beth ddigwyddodd.

## EXIT STATUS

Sero pan orffennodd y recordiad, gan gynnwys un a ddaeth i ben pan lenwodd y
ddisg. Nid sero pan na allai ddechrau neu pan ddaeth i ben mewn unrhyw
ffordd arall; ysgrifennir y rheswm i'r allbwn gwall safonol.

## SEE ALSO

`desktop`
