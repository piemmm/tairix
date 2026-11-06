## NAME

recorder — enregistrer l'écran pour le bureau

## SYNOPSIS

`recorder SESSION`

## DESCRIPTION

Enregistre l'écran dans un film tant que le bureau le lui demande. Il ne se
lance pas à la main : le bureau le démarre quand un enregistrement commence,
et il s'arrête quand l'enregistrement prend fin.

Le programme qui le démarre désigne par `SESSION` la session du bureau pour
laquelle il enregistre.

Pour enregistrer, appuyez sur `Meta+Shift+5` pour ouvrir la barre de
capture, choisissez l'écran entier, une fenêtre ou une zone, puis appuyez
sur **Record** (Enregistrer). Un raccourci de capture, l'indicateur
d'enregistrement dans la zone de notification ou la durée choisie dans
**Options** l'arrête.

Un enregistrement est sauvegardé sous le nom
`Screen Recording YYYY-MM-DD at HH.MM.SS.webm` dans le dossier Desktop ou
dans `Home/Videos`, selon ce que choisit **Options**. C'est un film WebM :
son image est une vidéo VP8 d'au plus trente images par seconde, prises
seulement quand quelque chose change à l'écran, et son son est en Opus.

**Sound** (Son) dans **Options** choisit ce que l'enregistrement entend :
rien, le microphone, le son propre du bureau, ou les deux mélangés.
L'enregistrement avec son d'un administrateur commence aussitôt. Celui de
tout autre compte demande d'abord le mot de passe d'un administrateur, puis
l'enregistreur fonctionne en tant que cet administrateur pour ce seul
enregistrement ; une annulation n'enregistre rien, et une notification le
signale.

Quand le disque est plein, l'enregistrement s'arrête là et il est conservé.
Quand l'enregistreur échoue, ce qu'il a enregistré est conservé et une
notification indique ce qui s'est passé.

## EXIT STATUS

Zéro quand l'enregistrement s'est terminé, y compris quand le disque s'est
rempli. Non nul quand il n'a pas pu commencer ou s'est terminé de toute autre
manière ; la raison est écrite sur la sortie d'erreur.

## SEE ALSO

`desktop`
