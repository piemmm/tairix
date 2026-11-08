## NAME

Draw — éditeur de dessins vectoriels

## SYNOPSIS

`Draw`

## DESCRIPTION

Dessine et modifie des dessins vectoriels dans une fenêtre du bureau :
tracés, rectangles, ellipses, étoiles et polygones, lignes, courbes à main
levée et texte, en groupes et en calques, sur une page. Lancé avec un
document — depuis le gestionnaire de fichiers, depuis le bureau, ou en
déposant un fichier sur son icône dans la barre d'icônes — il ouvre une
fenêtre dessus. Lancé seul, il ouvre un nouveau dessin sur une page A4.
Chaque document est une fenêtre de l'unique programme ; fermer la dernière
le laisse dans la barre d'icônes, et la ligne Quitter de son menu d'icône y
met fin.

Draw ouvre le SVG, les fichiers Draw de RISC OS et le PDF, et enregistre un
dessin en SVG, en fichier Draw de RISC OS ou en PDF, au format que dit le nom
du fichier ; Enregistrer sous propose les trois. Un fichier Draw garde d'un
dessin ce qu'il peut, et un PDF se rouvre en un seul calque dont le texte est
en contours ; l'enregistrement le dit. D'un PDF s'ouvre une page, en dessin,
sa zone de recadrage pour page : un PDF de plusieurs pages demande d'abord
laquelle, en donnant la taille de chaque page en points, et un PDF protégé par
un mot de passe le demande, et dit quand celui donné est faux. Le texte qu'un
PDF compose dans ses polices n'est pas encore lu. Un dessin garde sa page et
ses unités, ses calques, groupes et découpes, chaque tracé, forme, texte et
image, les couleurs unies et les dégradés linéaires et radiaux, l'opacité de
chaque objet, les tirets, jointures, extrémités et pointes de flèche, les
repères et la grille, et le titre du dessin. Un fichier qui contient ce qu'un
dessin ne peut pas garder — effets de filtre, masques, motifs, marqueurs,
images illisibles, styles ou chemins de texte qu'un texte ne peut garder,
animation ou scripts entre autres — s'ouvre en lecture seule, la bande d'état
nommant ce qu'il contient, et Enregistrer demande un nouveau nom pour que
l'original ne soit jamais écrasé. Enregistrer une copie écrit le dessin en PDF
; en EPS, sa transparence rendue opaque, comme PostScript la peint ; en PNG,
JPEG ou TIFF, à la résolution de copie de la page et jusqu'à 8192 pixels sur
son plus grand côté ; en SVG ; ou en fichier Draw de RISC OS, en nommant ce
que le format n'a pas pu garder du dessin, comme les dégradés ; chaque fois
sans toucher au fichier du document.

La boîte à outils descend le long du bord gauche de la fenêtre, l'inspecteur
le long du bord droit, et la bande d'état court en bas : elle dit ce qu'a fait
la dernière commande, sinon comment s'emploie l'outil en main, et montre la
position du pointeur et la taille de la sélection dans les unités de la page,
et le zoom : appuyer sur le zoom ouvre un menu de zooms. Des règles dans les
unités de la page bordent le haut et la gauche du canevas, et des barres de
défilement sa droite et son bas. S'arrêter sur le bouton d'un outil le nomme,
avec la touche qui le choisit.

Les outils, et les touches qui les choisissent, sont : sélection (`V`), nœuds
(`A`), rectangle (`R`), ellipse (`E`), étoile et polygone (`S`), ligne (`L`),
plume (`B`), crayon (`P`), texte (`T`), dégradé (`G`), pipette (`I`), zoom
(`Z`) et main (`H`).

L'outil de sélection prend un objet d'un appui, un groupe en entier, et
Maj ajoute un objet à la sélection ou l'en retire. Un appui sur le canevas
vide trace un rectangle de sélection qui prend ce qui s'y trouve
entièrement, ou avec Alt tout ce qu'il touche. Faire glisser la sélection
la déplace, sur un seul axe avec Maj, et avec Alt en déplace une copie. Ses
huit poignées la mettent à l'échelle depuis la poignée opposée, un coin avec
Maj gardant ses proportions ; faire glisser juste à l'extérieur d'un coin la
fait tourner autour de son centre, avec Maj par pas de 15 degrés. Les
touches fléchées la déplacent d'un pixel, et de dix avec Maj.

L'outil nœuds montre les nœuds du tracé sélectionné et les poignées de ses
courbes : faites glisser un nœud pour le déplacer, ou une poignée pour
infléchir sa courbe, l'autre poignée d'un nœud lisse suivant dans
l'alignement. Les rectangles, ellipses, étoiles et lignes se tracent d'un
glissement : Maj rend les côtés égaux — un carré, un cercle, une ligne à un
multiple de 15 degrés — et Alt trace depuis le point où le glissement a
commencé. Une étoile se trace depuis son centre ; ses branches, son rayon
intérieur et le choix d'un polygone se règlent dans l'onglet Forme de
l'inspecteur, comme les coins d'un nouveau rectangle.

La plume pose un tracé nœud par nœud : cliquez pour un angle, ou faites
glisser pour un nœud lisse et sa courbe. Un clic sur le premier nœud ferme le
tracé, et Entrée, ou le choix d'un autre outil, le termine ouvert ; `Delete`
ou `Backspace` reprend le dernier nœud, et Échap abandonne le tracé. Le crayon
dessine à main levée, et sa ligne est ajustée en courbes lisses quand on le
relâche. L'outil texte commence un texte là où l'on clique, ou modifie le
texte cliqué : tapez, Entrée commence une ligne, les touches fléchées, Début
et Fin déplacent le curseur, et Échap, ou un clic ailleurs, termine la saisie
en une seule modification ; un texte laissé vide est supprimé. La pipette
donne à la sélection, et à ce qui sera dessiné ensuite, le style de l'objet
cliqué. L'outil dégradé pose un dégradé le long du glissement sur la sélection
ou, si rien n'est sélectionné, sur l'objet pressé : Maj le tourne par pas de
15°, et Alt, ou un dégradé déjà radial, le pose radial autour du point où le
glissement a commencé ; tant que l'outil est en main, le dégradé de l'objet
sélectionné s'affiche comme une ligne entre deux poignées. L'outil zoom
agrandit là où l'on clique, réduit avec Alt, et un cadre tracé avec lui
remplit la fenêtre. La main fait glisser le canevas, comme un glissement avec
le bouton du milieu ou avec Espace enfoncé, quel que soit l'outil.

Appuyé sur un texte, l'outil texte place le curseur là où il a été appuyé. Maj
avec les flèches, Origine et Fin, ou un glissé sur le texte, sélectionne des
caractères, et `Ctrl+A` les sélectionne tous ; ce qui est tapé alors prend
leur place, dans le style du premier d'entre eux. Tant que des caractères sont
sélectionnés, une police, une taille, le gras, l'italique, l'espacement des
lettres, l'élévation ou une couleur choisis dans l'inspecteur ne règlent
qu'eux ; sans sélection, ils règlent tout le texte, chacun de ses caractères.
L'onglet Forme tient l'espacement des lettres d'un texte, de combien ses
caractères sont élevés, en pourcentage de leur taille, et son interligne — ou,
pour un texte le long d'un tracé, à quelle distance le long du tracé il
commence —, et pendant la frappe l'inspecteur montre comment est réglé le
premier caractère sélectionné. Un dégradé peint toujours un texte entier.

Tracé ▸ Poser le texte sur le tracé, avec un texte et une forme sélectionnés,
pose le texte le long du contour de la forme, qui le rejoint : chaque
caractère se tient sur le tracé, tourné selon sa direction, un texte le long
d'un tracé est une seule ligne, et un caractère qui tombe au-delà d'une
extrémité n'est pas dessiné. Détacher du tracé remet le texte en lignes depuis
l'endroit où commençait son tracé et rend le tracé comme un objet à part.
Objet en tracé change un texte peint de plus d'une manière en un groupe d'un
tracé pour chaque manière. L'outil nœuds modifie le tracé le long duquel un
texte est posé comme tout autre tracé.

La molette fait défiler le canevas, en travers avec Maj ; avec Ctrl elle
zoome autour du pointeur, comme un pincement sur un pavé tactile ou un écran
tactile. Affichage ▸ Ajuster la page et Ajuster la sélection les font tenir
dans la fenêtre, et Taille réelle montre le dessin à 100 pour cent ; le zoom
va de 1 pour cent à 25600.

L'inspecteur a cinq onglets. Style règle le remplissage et le contour de la
sélection — et, si rien n'est sélectionné, de ce qui sera dessiné ensuite :
choisissez Remplissage ou Contour, puis Aucun ou une couleur du sélecteur de
couleur, qui offre un carré de saturation et de valeur à côté d'une bande de
teintes, une roue ou des curseurs, et les valeurs et l'opacité de la couleur ;
puis l'épaisseur du contour, ses jointures, en onglet, arrondies ou
biseautées, ses extrémités, droites, arrondies ou carrées, ses tirets, plein,
tireté, pointillé ou mixte, les pointes de flèche à son début et à sa fin —
chaque appui passe par aucune, triangle, flèche, cercle et losange —, la règle
de remplissage, non nulle ou pair-impair, et l'opacité de chaque objet
sélectionné. Calques liste les calques, le plus haut d'abord : appuyez sur
l'un pour y dessiner, et sur son bouton Affiché pour l'afficher ou le masquer
; au-dessus de la liste, Nouveau ajoute un calque, Supprimer supprime le
calque en main, Verrouiller le protège des modifications, et Monter et
Descendre le déplacent. Objet place la sélection par sa gauche et son haut et
la met à une largeur et une hauteur, l'aligne — par les bords gauches, les
centres ou les bords droits, ou par le haut, le milieu ou le bas — et répartit
trois objets ou plus régulièrement en travers ou vers le bas. Les objets
s'alignent dans leur sélection ; un seul objet, ou tout nombre avec Aligner
sur la page, dans la page. Forme règle les coins d'un rectangle, les branches
et le rayon intérieur d'une étoile et le choix d'un polygone, ainsi que la
famille du texte — sans empattement, à empattement ou à chasse fixe —, sa
taille, le gras, l'italique et l'alignement sur son origine. Un onglet plus
haut que la fenêtre défile. Une couleur en cours de choix, une épaisseur tapée
ou réglée pas à pas et une opacité s'affichent sur le canevas à mesure
qu'elles changent et deviennent une seule modification à annuler quand elles
se fixent ; Tab passe au champ suivant et Maj+Tab au précédent.

Dans Style, Linéaire et Radial peignent le remplissage ou le contour d'un
dégradé ajusté à chaque objet, de sa couleur au blanc, ou au noir là où elle
est claire. La barre en dessous montre le dégradé sur un damier, avec une
poignée pour chaque arrêt : appuyez sur une poignée pour prendre cet arrêt en
main — sa couleur revient alors au sélecteur de couleur — et faites-la glisser
le long de la barre ; appuyez sur la barre pour y ajouter un arrêt. Suppr, ou
Retirer l'arrêt, retire l'arrêt en main ; Gauche et Droite prennent l'arrêt
voisin, et avec Maj le déplacent d'un centième. Arrêt à place l'arrêt en main,
Angle tourne un dégradé linéaire, Étendre, Refléter et Répéter disent comment
un dégradé se poursuit au-delà de ses extrémités, et Inverser le fait courir
dans l'autre sens. Sous le sélecteur, les nuances — des gris, puis douze
teintes pleines, claires et sombres, puis celles du dessin lui-même — donnent
leur couleur à la couleur ou à l'arrêt en main ; Ajouter une nuance garde la
couleur en main avec le dessin et Retirer la nuance laisse partir l'une des
siennes. Dans Objet, Rotation tourne la sélection dans le sens antihoraire :
un objet jusqu'à l'angle donné, plusieurs ensemble de cet angle.

Page règle la largeur et la hauteur de la page dans son unité, et si elle est
en portrait ou en paysage ; l'unité — px, pt, pc, mm, cm ou in ; l'espacement
de la grille dans cette unité, et en combien de parties chaque côté de ses
carrés est divisé ; ppp de copie, la résolution à laquelle ses copies PNG,
JPEG et TIFF sont faites ; et le fond de la page, Aucun ou une couleur d'un
sélecteur de couleur qui lui est propre, affichée sur le canevas pendant qu'on
la choisit. Une page sans fond s'affiche comme du papier blanc, et sa copie en
PNG est transparente partout où rien n'est dessiné.

Un glissement depuis une règle pose un repère en travers ou vers le bas du
canevas ; faites glisser un repère pour le déplacer, ou ramenez-le sur une
règle pour le supprimer. Affichage ▸ Grille montre la grille de la page. Tant
que Magnétisme est actif, le pointeur s'aimante quand on trace une forme, pose
un nœud à la plume ou déplace ou met à l'échelle la sélection : aux repères,
aux bords et au centre de la page, et à la grille quand elle est affichée.
Affichage ▸ Contours montre chaque objet par son contour, ce qui rend lisible
un dessin chargé. Mise en page, dans le menu Page, affiche l'onglet Page, et
le menu règle le papier de la page — A4, A3, A5, A6, A2, A1, A0, B5, B4, US
Letter, US Legal, Tabloïd, Exécutive, deux tailles d'écran et une icône —, la
tourne entre portrait et paysage, lui donne le fond de l'onglet Page ou aucun,
et règle ses unités : pixels, points, picas, millimètres, centimètres ou
pouces.

Objet ▸ Grouper réunit la sélection en un groupe et Dégrouper le défait ;
Mettre au premier plan, Monter, Descendre et Mettre à l'arrière-plan
réordonnent les objets dans leur calque ; Retourner et Tourner font une
symétrie de la sélection ou la tournent d'un quart de tour. Verrouiller et
Masquer mettent des objets hors de portée, et Tout déverrouiller et Tout
afficher dans le calque ramènent ceux du calque en main. Le menu Tracé
réunit les formes sélectionnées en une seule (Union), découpe celles du
dessus dans la plus basse (Différence), garde où elles se chevauchent
(Intersection) ou où elles ne se chevauchent pas (Exclusion), change formes
et textes en tracés dont les nœuds se modifient, et change les contours en
formes remplies. Ces opérations se calculent à l'écart de la fenêtre, qui
reste libre pendant ce temps ; un dessin modifié avant qu'elles aboutissent
garde sa modification, et l'opération n'est pas appliquée.

Objet ▸ Découper selon l'objet du dessus n'affiche le reste de la sélection
qu'à l'intérieur de son objet le plus haut, et Libérer la découpe affiche de
nouveau en entier un groupe découpé, l'objet qui le découpait replacé
au-dessus de ce qu'il contenait. Tracé ▸ Diviser coupe les formes
sélectionnées en morceaux selon leurs chevauchements. Dilater et Contracter
agrandissent ou réduisent chaque forme sélectionnée du Décalage de l'onglet
Forme, Simplifier retrace chacune avec aussi peu de nœuds qu'il en faut pour
rester dans le Lissage de l'onglet, et Inverser le sens fait parcourir un
tracé dans l'autre sens, ses pointes de flèche et ses tirets le suivant.

Chaque modification est une étape à annuler, et Annuler et Rétablir nomment
l'étape qu'ils prennent ; jusqu'à 1000 étapes sont gardées, moins quand la
mémoire manque. Copier met la sélection dans le presse-papiers en SVG, et
Coller ajoute ce que contient le presse-papiers au calque en main, à sa
place sur la page, sélectionné.

Fichier ▸ Placer une image (`Ctrl+Shift+P`) demande une image — un fichier
PNG, JPEG, GIF, BMP, TIFF, WebP, d'icône Windows ou OpenRaster, ou un fichier
de sprites RISC OS, la première page ou le premier sprite d'un fichier qui en
contient plusieurs — et la pose dans le calque en main en une étape,
sélectionnée et nommée d'après son fichier, au milieu de ce que montre le
canevas : à sa propre taille, ou réduite aux quatre cinquièmes du canevas là
où elle est plus grande. Le fichier est seulement lu, jamais modifié. Une
image se déplace, se met à l'échelle, tourne, se découpe et se copie comme
tout objet ; elle n'a ni style ni nœuds propres, et là où des formes sont
unies, découpées ou divisées, elle y prend part comme son rectangle. Elle est
enregistrée avec le dessin : le SVG l'incorpore, une copie PDF ou EPS la
dessine et un fichier Draw la garde comme sprite ou comme JPEG. Les images
d'un dessin tiennent ensemble au plus 268 millions de pixels, chacune comptée
une fois, si souvent qu'elle soit montrée ; une image qui les porterait
au-delà est refusée, et la bande d'état dit pourquoi. Une image glissée depuis
le gestionnaire de fichiers ou le bureau et lâchée sur le canevas est placée
de même, centrée là où elle a été lâchée ; tout autre fichier déposé sur la
fenêtre s'ouvre, ou est refusé, comme s'il était déposé sur l'icône de Draw.

Réglages, dans le menu de la barre d'icônes, ouvre la fenêtre des réglages de
Draw : l'outil avec lequel une nouvelle fenêtre commence, et si elle montre la
grille, aimante et ne dessine que les contours ; le papier, l'orientation, les
unités et la grille d'un nouveau dessin ; le fond et le contour des nouveaux
objets, chacun aucun ou une couleur, et l'épaisseur du contour ; de combien
les touches fléchées déplacent, d'un pixel sauf réglage ; à quelle distance un
duplicata se tient de ce qu'il copie ; et à quel point le pointeur doit
s'approcher d'une poignée, d'un nœud ou d'un repère pour le saisir. Un
changement s'applique aussitôt et reste pour la fois suivante ; Rétablir les
valeurs par défaut les remet tous.

Draw ne détient aucune capacité sur le système de fichiers. Il ne modifie que
le fichier qu'on lui a remis : un fichier que l'utilisateur peut changer lui
est remis inscriptible et Enregistrer l'écrit en retour, et tout autre s'ouvre
en lecture seule et Enregistrer demande où l'enregistrer. Les documents, les
dessins collés depuis le presse-papiers et les images placées sont lus dans un
processus de travail séparé sans aucune portée, si bien qu'un fichier hostile
ne peut toucher à rien de ce que Draw peut atteindre.

Appuyer sur le bouton secondaire (droit) de la souris n'importe où dans la
fenêtre ouvre son menu : Fichier, Page, Édition, Objet, Tracé, Calque,
Affichage et Outils, chacun un sous-menu. La fenêtre n'a pas de barre de
menus. Fermer une fenêtre, ou quitter, avec des modifications non
enregistrées demande d'abord. Fermer une fenêtre avec le bouton secondaire
sur son bouton de fermeture montre aussi son fichier dans le gestionnaire de
fichiers, dans le dossier qui le contient.

* `Ctrl+N` — une nouvelle fenêtre ; `Ctrl+O` — ouvrir un fichier ; `Ctrl+Shift+P` — placer une image
* `Ctrl+S` — enregistrer ; `Ctrl+Shift+S` — enregistrer sous ; `Ctrl+Shift+E` — enregistrer une copie
* `Ctrl+W` — fermer la fenêtre
* `Ctrl+Z` — annuler ; `Ctrl+Shift+Z` ou `Ctrl+Y` — rétablir
* `Ctrl+X`, `Ctrl+C`, `Ctrl+V` — couper, copier, coller ; `Ctrl+D` — dupliquer
* `Delete` ou `Backspace` — supprimer la sélection
* `Ctrl+A` — tout sélectionner ; `Ctrl+Shift+A` — ne rien sélectionner ; `Escape` — ne rien sélectionner, ou abandonner le tracé en cours
* `Ctrl+G` — grouper ; `Ctrl+Shift+G` — dégrouper
* `Page Up` / `Page Down` — monter ou descendre ; `Home` / `End` — mettre au premier plan ou à l'arrière-plan
* `Ctrl+[` / `Ctrl+]` — tourner à gauche ou à droite
* `Ctrl+L` — verrouiller ; `Ctrl+Shift+L` — tout déverrouiller dans le calque
* `Ctrl+H` — masquer ; `Ctrl+Shift+H` — tout afficher dans le calque
* `Ctrl+Shift+C` — objet en tracé
* `Ctrl+Shift+N` — un nouveau calque ; `Ctrl+Shift+Page Up` / `Ctrl+Shift+Page Down` — monter ou descendre le calque
* `Ctrl+=` ou `Ctrl++` / `Ctrl+-` — zoomer ou dézoomer ; `Ctrl+1` — taille réelle ; `Ctrl+0` — ajuster la page
* `Ctrl+'` — afficher ou masquer la grille ; `%` — magnétisme ou non
* `V`, `A`, `R`, `E`, `S`, `L`, `B`, `P`, `T`, `G`, `I`, `Z`, `H` — les outils, dans l'ordre
* touches fléchées — déplacer la sélection autant que le disent les réglages, d'un pixel sauf réglage ; avec `Shift`, dix fois plus
* `Space` — enfoncé, faire glisser le canevas avec n'importe quel outil
* `Ctrl` + molette — zoomer autour du pointeur ; `Shift` + molette — défiler en travers

## OPTIONS

`-h`, `-?`, `--help`
: Écrit cette aide sur la sortie standard et termine.

## EXIT STATUS

Zéro après Quitter. Non nul quand le canal de fenêtre, la boîte aux lettres
d'événements ou la session du bureau a été refusé ; la raison est indiquée
sur la sortie d'erreur standard.
