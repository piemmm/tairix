## NAME

Paint — éditeur graphique d'images et de sprites

## SYNOPSIS

`Paint`

## DESCRIPTION

Peint et modifie des images dans une fenêtre de bureau, pixel par pixel ou
avec des pinceaux et des formes. Lancé avec un document — depuis le
gestionnaire de fichiers, depuis le bureau, ou en déposant un fichier sur
son icône dans la barre d'icônes — il ouvre une fenêtre sur ce document.
Lancé seul, il ouvre une nouvelle image blanche. Chaque document est une
fenêtre de l'unique programme ; fermer la dernière le laisse dans la barre
d'icônes, et la ligne Quitter de son menu d'icône y met fin.

Il ouvre tous les formats d'image que lit le système : PNG, JPEG, GIF, BMP,
TIFF, WebP, icônes Windows et fichiers de sprites RISC OS. Il écrit les
formats PNG, JPEG et les fichiers de sprites ; une image lue dans un autre
format est enregistrée comme un nouveau fichier. Un PNG garde sa palette,
et un JPEG est écrit à la qualité réglée par Qualité JPEG dans le menu
Fichier.

Un fichier de sprites contient un nombre quelconque de sprites, chacun avec
son nom, son mode d'écran, sa palette et son masque. Chaque profondeur est
modifiée telle qu'elle est stockée : 2, 4, 16 et 256 couleurs, et des
millions de couleurs. Un sprite sans palette propre affiche les couleurs du
bureau RISC OS — en 16 couleurs, la couleur n est la couleur Wimp n ; en 2
couleurs, les couleurs Wimp 0 et 7 ; en 4 couleurs, les couleurs Wimp 0, 2,
4 et 7 ; en 256 couleurs, l'arrangement de teintes de RISC OS — jamais une
palette de PC. Un sprite dont les pixels sont plus hauts que larges, comme
en mode 12, est affiché ainsi. Un sprite que l'éditeur ne sait pas lire, un
sprite CMYK par exemple, est conservé exactement tel quel et réenregistré
sans changement. Le menu Sprites permet d'aller à un sprite, d'en ajouter,
de le copier, de le renommer, de le supprimer et de réordonner les sprites.

Le bouton principal (gauche) peint avec la couleur principale et le bouton
du milieu avec la couleur secondaire ; en maintenant Alt, on prélève une
couleur à la place. Les outils sont : sélection, crayon, pinceau,
aérographe, gomme, remplissage, pipette, ligne, rectangle et ellipse ; le
panneau à côté de l'image contient la palette de l'image ou les couleurs du
bureau et les réglages de l'outil en cours. Le volet des couleurs à droite
contient les couleurs principale et secondaire et un sélecteur de couleur
pour celle des deux qui est choisie : cliquer sur une couleur la choisit,
puis on la règle par teinte, saturation et valeur, par rouge, vert et bleu,
par sa notation hexadécimale et, si l'image admet la transparence, par son
opacité. La couleur qu'elle avait reste à côté, et un clic la rétablit.
Dans une image à palette, les couleurs en sont les entrées : le sélecteur
modifie donc la palette, et chaque modification est un changement à
annuler d'un coup. Maintenir Maj trace un carré, un cercle
ou une ligne à un multiple de 45 degrés.

Avec l'outil de sélection, faites glisser pour délimiter une partie de
l'image, puis faites glisser la sélection pour la déplacer ; elle flotte
jusqu'à ce qu'elle soit posée, et son déplacement s'annule en une seule
fois. Les images copiées passent par le presse-papiers au format PNG, et ce
qui est collé flotte jusqu'à ce qu'il soit posé.

Le programme ne détient aucune capacité sur le système de fichiers. Il ne
modifie que le fichier qui lui a été remis. Un fichier que l'utilisateur
peut modifier est remis en écriture, et Enregistrer l'y réécrit ; tout
autre est en lecture seule, et Enregistrer demande où enregistrer une
copie. Les images, et celles collées depuis le presse-papiers, sont
décodées dans un processus de travail séparé sans aucune portée, et chaque
document en reçoit un nouveau : un fichier hostile ne peut atteindre rien
de ce que le programme peut atteindre.

Appuyer sur le bouton secondaire (droit) de la souris n'importe où dans la
fenêtre ouvre son menu : Couper, Copier, Coller, Tout sélectionner et
Désélectionner, puis Fichier, Édition, Image, Couleurs, Sprites, Affichage
et Outils, chacun ouvrant son sous-menu. La fenêtre n'a pas de barre de
menus. Fermer une fenêtre ou quitter avec des modifications non
enregistrées demande d'abord confirmation.

* `Ctrl+N` — une nouvelle image ; `Ctrl+O` — ouvrir un fichier
* `Ctrl+S` — enregistrer ; `Ctrl+Shift+S` — enregistrer sous
* `Ctrl+W` — fermer la fenêtre
* `Ctrl+Z` — annuler ; `Ctrl+Shift+Z` ou `Ctrl+Y` — rétablir
* `Ctrl+X`, `Ctrl+C`, `Ctrl+V` — couper, copier, coller
* `Ctrl+A` — tout sélectionner ; `Ctrl+D` — désélectionner
* `Enter` — poser une sélection flottante ; `Escape` — la remettre en place
* `Delete` — effacer la sélection
* `Ctrl+Shift+X` — rogner à la sélection
* `Ctrl+R` — redimensionner ; `Ctrl+Shift+R` — taille du canevas
* `Ctrl+[` / `Ctrl+]` — tourner à gauche ou à droite
* `Ctrl+I` — inverser les couleurs
* `S`, `P`, `B`, `A`, `E`, `F`, `I`, `L`, `R`, `O` — les outils, dans l'ordre
* `X` — échanger les couleurs principale et secondaire
* `Tab` — dans le volet des couleurs et à travers ses parties ; `Escape` — retour à l'image
* `+` / `-` — zoomer ou dézoomer ; `1` — taille réelle ; `Ctrl+0` — ajuster
* `Ctrl` + molette — agrandir ou réduire autour du pointeur
* Pincer à deux doigts — agrandir ou réduire en continu ; sur un écran tactile, l'image suit les doigts
* `G` — afficher ou masquer la grille entre les pixels
* `Page Up` / `Page Down` — le sprite précédent ou suivant
* touches fléchées — déplacer une sélection flottante d'un pixel ; avec `Shift`, de dix

## OPTIONS

`-h`, `-?`, `--help`
: Écrit cette aide sur la sortie standard et termine.

## EXIT STATUS

Zéro après Quitter. Non nul lorsque le canal de fenêtre, la boîte aux
lettres d'événements ou la session de bureau a été refusé ; la raison est
indiquée sur la sortie d'erreur standard.
