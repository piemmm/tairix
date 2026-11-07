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

Draw ouvre et enregistre le SVG. Un dessin garde sa page et ses unités, ses
calques, groupes et découpes, chaque tracé, forme et texte, les couleurs
unies et les dégradés linéaires et radiaux, l'opacité de chaque objet, les
tirets, jointures, extrémités et pointes de flèche, les repères et la
grille, et le titre du dessin. Un fichier qui contient ce qu'un dessin ne
peut pas garder — effets de filtre, masques, motifs, marqueurs, images
incorporées, styles de texte mêlés, animation ou scripts entre autres —
s'ouvre en lecture seule, la bande d'état nommant ce qu'il contient, et
Enregistrer demande un nouveau nom pour que l'original ne soit jamais
écrasé. Enregistrer une copie écrit le dessin en PDF, en PNG — la page à sa
propre taille, jusqu'à 8192 pixels sur son plus grand côté — ou en SVG, sans
toucher au fichier du document.

La boîte à outils descend le long du bord gauche de la fenêtre, l'inspecteur
le long du bord droit, et la bande d'état court en bas : elle montre la
position du pointeur et la taille de la sélection dans les unités de la
page, et dit ce qu'a fait la dernière commande. En haut, la barre dit comment
s'emploie l'outil en main et montre le zoom : appuyer sur le zoom ouvre un
menu de zooms. Des règles dans les unités de la page bordent le haut et la
gauche du canevas, et des barres de défilement sa droite et son bas.
S'arrêter sur le bouton d'un outil le nomme, avec la touche qui le choisit.

Les outils, et les touches qui les choisissent, sont : sélection (`V`),
nœuds (`A`), rectangle (`R`), ellipse (`E`), étoile et polygone (`S`),
ligne (`L`), plume (`B`), crayon (`P`), texte (`T`), pipette (`I`),
zoom (`Z`) et main (`H`).

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
glisser pour un nœud lisse et sa courbe. Un clic sur le premier nœud ferme
le tracé, et Entrée, ou le choix d'un autre outil, le termine ouvert ;
`Delete` ou `Backspace` reprend le dernier nœud, et Échap abandonne le
tracé. Le crayon dessine à main levée, et sa ligne est ajustée en courbes
lisses quand on le relâche. L'outil texte commence un texte là où l'on
clique, ou modifie le texte cliqué : tapez, Entrée commence une ligne, les
touches fléchées, Début et Fin déplacent le curseur, et Échap, ou un clic
ailleurs, termine la saisie en une seule modification ; un texte laissé vide
est supprimé. La pipette donne à la sélection, et à ce qui sera dessiné
ensuite, le style de l'objet cliqué. L'outil zoom agrandit là où l'on
clique, réduit avec Alt, et un cadre tracé avec lui remplit la fenêtre. La
main fait glisser le canevas, comme un glissement avec le bouton du milieu
ou avec Espace enfoncé, quel que soit l'outil.

La molette fait défiler le canevas, en travers avec Maj ; avec Ctrl elle
zoome autour du pointeur, comme un pincement sur un pavé tactile ou un écran
tactile. Affichage ▸ Ajuster la page et Ajuster la sélection les font tenir
dans la fenêtre, et Taille réelle montre le dessin à 100 pour cent ; le zoom
va de 1 pour cent à 25600.

L'inspecteur a quatre onglets. Style règle le remplissage et le contour de
la sélection — et, si rien n'est sélectionné, de ce qui sera dessiné
ensuite : choisissez Remplissage ou Contour, puis Aucun ou une couleur du
sélecteur de couleur, qui offre un carré de saturation et de valeur à côté
d'une bande de teintes, une roue ou des curseurs, et les valeurs et
l'opacité de la couleur ; puis l'épaisseur du contour, ses jointures, en
onglet, arrondies ou biseautées, ses extrémités, droites, arrondies ou
carrées, ses tirets, plein, tireté, pointillé ou mixte, les pointes de
flèche à son début et à sa fin — chaque appui passe par aucune, triangle,
flèche, cercle et losange —, la règle de remplissage, non nulle ou
pair-impair, et l'opacité de chaque objet sélectionné. Calques liste les
calques, le plus haut d'abord : appuyez sur l'un pour y dessiner, et sur son
bouton Affiché pour l'afficher ou le masquer ; au-dessus de la liste,
Nouveau ajoute un calque, Supprimer supprime le calque en main, Verrouiller
le protège des modifications, et Monter et Descendre le déplacent. Objet
place la sélection par sa gauche et son haut et la met à une largeur et une
hauteur, l'aligne — par les bords gauches, les centres ou les bords droits,
ou par le haut, le milieu ou le bas — et répartit trois objets ou plus
régulièrement en travers ou vers le bas. Les objets s'alignent dans leur
sélection ; un seul objet, ou tout nombre avec Aligner sur la page, dans la
page. Forme règle les coins d'un rectangle, les branches et le rayon
intérieur d'une étoile et le choix d'un polygone, ainsi que la famille du
texte — sans empattement, à empattement ou à chasse fixe —, sa taille, le
gras, l'italique et l'alignement sur son origine. Un onglet plus haut que la
fenêtre défile. Une couleur en cours de choix, une épaisseur tapée ou réglée
pas à pas et une opacité s'affichent sur le canevas à mesure qu'elles
changent et deviennent une seule modification à annuler quand elles se
fixent ; Tab passe au champ suivant et Maj+Tab au précédent.

Un glissement depuis une règle pose un repère en travers ou vers le bas du
canevas ; faites glisser un repère pour le déplacer, ou ramenez-le sur une
règle pour le supprimer. Affichage ▸ Grille montre la grille de la page.
Tant que Magnétisme est actif, le pointeur s'aimante quand on trace une
forme, pose un nœud à la plume ou déplace ou met à l'échelle la sélection :
aux repères, aux bords et au centre de la page, et à la grille quand elle
est affichée. Affichage ▸ Contours montre chaque objet par son contour, ce
qui rend lisible un dessin chargé. Le menu Page règle le papier de la page —
A4, A3, A5, A6, A2, A1, A0, B5, B4, US Letter, US Legal, Tabloïd,
Exécutive, deux tailles d'écran et une icône —, la tourne entre portrait et
paysage, lui donne un fond blanc ou aucun, et règle ses unités : pixels,
points, picas, millimètres, centimètres ou pouces.

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

Chaque modification est une étape à annuler, et Annuler et Rétablir nomment
l'étape qu'ils prennent ; jusqu'à 1000 étapes sont gardées, moins quand la
mémoire manque. Copier met la sélection dans le presse-papiers en SVG, et
Coller ajoute ce que contient le presse-papiers au calque en main, à sa
place sur la page, sélectionné.

Draw ne détient aucune capacité sur le système de fichiers. Il ne modifie
que le fichier qu'on lui a remis : un fichier que l'utilisateur peut changer
lui est remis inscriptible et Enregistrer l'écrit en retour, et tout autre
s'ouvre en lecture seule et Enregistrer demande où l'enregistrer. Les
documents, et les dessins collés depuis le presse-papiers, sont lus dans un
processus de travail séparé sans aucune portée, si bien qu'un fichier
hostile ne peut toucher à rien de ce que Draw peut atteindre.

Appuyer sur le bouton secondaire (droit) de la souris n'importe où dans la
fenêtre ouvre son menu : Fichier, Page, Édition, Objet, Tracé, Calque,
Affichage et Outils, chacun un sous-menu. La fenêtre n'a pas de barre de
menus. Fermer une fenêtre, ou quitter, avec des modifications non
enregistrées demande d'abord. Fermer une fenêtre avec le bouton secondaire
sur son bouton de fermeture montre aussi son fichier dans le gestionnaire de
fichiers, dans le dossier qui le contient.

* `Ctrl+N` — une nouvelle fenêtre ; `Ctrl+O` — ouvrir un fichier
* `Ctrl+S` — enregistrer ; `Ctrl+Shift+S` — enregistrer sous ; `Ctrl+Shift+E` — enregistrer une copie en PDF, PNG ou SVG
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
* `V`, `A`, `R`, `E`, `S`, `L`, `B`, `P`, `T`, `I`, `Z`, `H` — les outils, dans l'ordre
* touches fléchées — déplacer la sélection d'un pixel ; avec `Shift`, de dix
* `Space` — enfoncé, faire glisser le canevas avec n'importe quel outil
* `Ctrl` + molette — zoomer autour du pointeur ; `Shift` + molette — défiler en travers

## OPTIONS

`-h`, `-?`, `--help`
: Écrit cette aide sur la sortie standard et termine.

## EXIT STATUS

Zéro après Quitter. Non nul quand le canal de fenêtre, la boîte aux lettres
d'événements ou la session du bureau a été refusé ; la raison est indiquée
sur la sortie d'erreur standard.
