## NAME

Paint — éditeur graphique d'images et de sprites

## SYNOPSIS

`Paint`

## DESCRIPTION

Peint et modifie des images dans une fenêtre du bureau, pixel par pixel ou
avec des pinceaux et des formes. Lancé avec un document — depuis le
gestionnaire de fichiers, depuis le bureau, ou en déposant un fichier sur
son icône dans la barre d'icônes — il ouvre une fenêtre dessus. Lancé seul,
il ouvre une nouvelle image blanche. Chaque document est une fenêtre de
l'unique programme ; fermer la dernière le laisse dans la barre d'icônes, et
la ligne Quitter de son menu d'icône y met fin.

Il ouvre tous les formats d'image que le système lit : PNG, JPEG, GIF, BMP,
TIFF, WebP, icônes Windows, fichiers de sprites RISC OS et OpenRaster. Il
écrit PNG, JPEG, GIF, BMP, TIFF, fichiers de sprites et OpenRaster ; une
image lue dans un autre format, ou dans un fichier qui contient plus que son
image, comme un profil de couleur, est enregistrée comme nouveau fichier.
Nouvelle image demande d'abord à quel format l'image est destinée et
propose les couleurs que ce format contient. Enregistrer sous demande le
format et ses réglages propres — la qualité d'un JPEG, si un GIF est
entrelacé, la compression d'un TIFF — et dit ce que le format ne peut pas
garder, avant de demander où. Une palette est gardée partout où le format en
contient une, de même que la résolution d'une image.

Une image peut être faite de calques, le plus bas d'abord, chacun avec un
nom, une opacité et l'indication s'il est visible ; on peint sur un calque à
la fois, et la fenêtre les montre superposés. OpenRaster garde les calques ;
tout autre format les reçoit superposés. Le menu Calques ajoute, copie,
supprime, monte et descend des calques, fusionne un calque avec celui du
dessous et aplatit l'image, et ses Propriétés du calque renomment un calque
et règlent la part qui en est visible. Les réglages, remplissages et traits
changent le calque peint ; les rotations, symétries, redimensionnements et
recadrages changent tous les calques. Une image à palette n'a qu'un calque.

Un fichier de sprites contient un nombre quelconque de sprites, chacun avec
son nom, son mode d'écran, sa palette et son masque. Chaque profondeur est
modifiée telle qu'elle est stockée : 2, 4, 16 et 256 couleurs et des
millions de couleurs. Un sprite sans palette propre montre les couleurs du
bureau RISC OS — pour 16 couleurs, la couleur n est la couleur Wimp n ; pour
2 couleurs, les couleurs Wimp 0 et 7 ; pour 4 couleurs, les couleurs Wimp 0,
2, 4 et 7 ; pour 256 couleurs, la disposition de teintes de RISC OS —
jamais une palette de PC. Un sprite dont les pixels sont plus hauts que
larges, comme en mode 12, est montré ainsi. Un sprite que cet éditeur ne
peut pas lire, comme un sprite CMJN, est gardé exactement tel quel et
réécrit sans changement. Le menu Sprites va à des sprites, en ajoute, en
copie, les renomme, les supprime et les réordonne. Un TIFF contient un
nombre quelconque de pages ; pour lui, le menu Pages va à des pages, en
ajoute, en copie, les supprime et les réordonne.

Le bouton principal (gauche) peint avec la couleur principale et le bouton
du milieu avec la couleur secondaire ; en tenant Alt, on prélève plutôt une
couleur, telle que les calques la montrent. Les outils de la boîte à outils
à gauche sont sélection, crayon, pinceau, aérographe, gomme, clonage,
remplissage, dégradé, pipette, texte, ligne, rectangle, ellipse, polygone,
recadrage, main et loupe. La barre du haut nomme l'outil utilisé et contient
ses réglages — la taille, la dureté, l'opacité, le flux et l'espacement d'un
pinceau, la tolérance d'un remplissage, la forme d'un dégradé, la taille du
texte, les coins d'un rectangle — saisis ou réglés avec les flèches, et les
boutons qui agrandissent et montrent la grille des pixels ; la bande de
palette sous l'image contient la palette de l'image, ou les couleurs du
bureau. L'aérographe continue de pulvériser tant qu'il est tenu immobile. En
tenant Maj, on trace un carré, un cercle ou une ligne à un multiple de 45
degrés.

L'outil de sélection délimite un rectangle, une ellipse, un lasso à main
levée, un polygone cliqué coin par coin, ou avec la baguette magique les
pixels reliés à l'un d'eux par des couleurs semblables. Maj ajoute à la
sélection, Alt en retire, et les deux ne gardent que ce qu'elles ont en
commun ; Adoucir adoucit son bord. Tant qu'une sélection est tenue, chaque
outil, remplissage et réglage y est tenu. Faire glisser à l'intérieur la
soulève et la déplace : elle flotte jusqu'à ce qu'elle soit posée, et la
déplacer est une seule modification à annuler. Les images copiées passent
par le presse-papiers en PNG, et ce qui est collé flotte jusqu'à ce qu'il
soit posé.

L'outil de clonage peint ce qui se trouve ailleurs dans l'image : Alt-clic
là où copier, puis peindre. L'outil de dégradé fond la couleur principale
dans la secondaire le long d'un glissement, en bandes ou en anneaux. L'outil
texte place les mots saisis là où l'on clique ; Entrée commence une nouvelle
ligne, un autre clic ou un autre outil les pose, et Échap les abandonne. Les
coins de l'outil polygone sont cliqués à tour de rôle, et un clic sur le
premier ou Entrée le ferme. L'outil de recadrage marque la partie à garder,
ses poignées déplacent ses bords, et Entrée recadre. La main fait glisser
l'image dans la fenêtre, comme Espace avec n'importe quel outil ; la loupe
agrandit à un clic, ou avec Alt réduit, et un cadre tracé remplit la
fenêtre.

Le menu Réglages change la luminosité et le contraste, la teinte et la
saturation et les niveaux, postérise, applique un seuil, désature, rend
flou, accentue, pixelise, ajoute du bruit et trouve les contours ; chacun se
montre sur l'image à mesure que ses réglages bougent et n'est gardé que s'il
est appliqué. Sur une image à palette, un réglage change sa palette, et ceux
qui ont besoin des couleurs voisines ne sont pas proposés.

Le volet des couleurs à droite contient les couleurs principale et
secondaire et un sélecteur de couleur pour celle des deux qui est choisie :
un clic sur une couleur la choisit, puis on la règle par teinte, saturation
et valeur, par rouge, vert et bleu, par son écriture hexadécimale et, là où
l'image contient de la transparence, par son opacité. La couleur qu'elle
avait se tient à côté, et un clic la rétablit. Sur une image à palette, les
couleurs sont ses entrées, le sélecteur modifie donc la palette, et chaque
modification est une seule modification à annuler.

Le programme ne détient aucune autorisation sur le système de fichiers. Il
ne modifie que le fichier qui lui a été remis. Un fichier que l'utilisateur
peut changer est remis en écriture, et Enregistrer le réécrit ; tout autre
est en lecture seule, et Enregistrer demande où enregistrer une copie. Les
images, et celles collées depuis le presse-papiers, sont décodées dans un
processus de travail séparé sans aucune portée, et chaque document en reçoit
un nouveau : un fichier hostile ne peut rien atteindre de ce que le
programme atteint.

Le bouton secondaire (droit) de la souris, n'importe où dans la fenêtre,
ouvre son menu : Couper, Copier, Coller, Tout sélectionner et Désélectionner,
puis Fichier, Édition, Image, Calques, Couleurs, Réglages, Sprites ou
Pages, Affichage et Outils, chacun ouvrant son propre sous-menu. La fenêtre
n'a pas de barre de menus. Fermer une fenêtre ou quitter avec des
modifications non enregistrées demande d'abord.

* `Ctrl+N` — une nouvelle image ; `Ctrl+O` — ouvrir un fichier
* `Ctrl+S` — enregistrer ; `Ctrl+Shift+S` — enregistrer sous
* `Ctrl+W` — fermer la fenêtre
* `Ctrl+Z` — annuler ; `Ctrl+Shift+Z` ou `Ctrl+Y` — rétablir
* `Ctrl+X`, `Ctrl+C`, `Ctrl+V` — couper, copier, coller
* `Ctrl+A` — tout sélectionner ; `Ctrl+D` — désélectionner
* `Enter` — poser une sélection flottante, fermer un polygone ou recadrer ; `Escape` — revenir en arrière
* `Delete` — effacer la sélection ; `Alt+Backspace` — la remplir de la couleur principale
* `Ctrl+Shift+X` — recadrer sur la sélection
* `Ctrl+R` — redimensionner ; `Ctrl+Shift+R` — taille du canevas
* `Ctrl+[` / `Ctrl+]` — tourner à gauche ou à droite
* `Ctrl+I` — inverser les couleurs
* `Ctrl+Shift+N` — un nouveau calque ; `Ctrl+E` — fusionner vers le bas ; `Ctrl+Shift+E` — aplatir
* `Ctrl+Page Up` / `Ctrl+Page Down` — peindre sur le calque du dessus ou du dessous
* `Ctrl+Shift+Page Up` / `Ctrl+Shift+Page Down` — monter ou descendre le calque
* `S`, `P`, `B`, `A`, `E`, `C`, `F`, `D`, `I`, `T`, `L`, `R`, `O`, `Y`, `K`, `H`, `Z` — les outils, dans l'ordre
* `Space` — tenu, faire glisser l'image avec n'importe quel outil
* `X` — échanger les couleurs principale et secondaire
* `Tab` — à travers les réglages de l'outil, la bande de palette et le volet des couleurs ; `Shift+Tab` — en sens inverse ; `Escape` — retour à l'image
* `+` / `-` — agrandir ou réduire ; `1` — taille réelle ; `Ctrl+0` — ajuster
* `Ctrl` + molette — agrandir ou réduire autour du pointeur
* Pincer à deux doigts — agrandir ou réduire en continu ; sur un écran tactile, l'image suit les doigts
* `G` — afficher ou masquer la grille entre les pixels
* `Page Up` / `Page Down` — le sprite précédent ou suivant, la page précédente ou suivante
* touches fléchées — déplacer une sélection flottante d'un pixel ; avec `Shift`, de dix ; dans la bande de palette, parcourir ses couleurs

## OPTIONS

`-h`, `-?`, `--help`
: Écrit cette aide sur la sortie standard et termine.

## EXIT STATUS

Zéro après Quitter. Non nul quand le canal de fenêtre, la boîte aux lettres
d'événements ou la session du bureau a été refusé ; la raison est indiquée
sur la sortie d'erreur standard.
