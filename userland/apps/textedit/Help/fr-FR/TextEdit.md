## NAME

TextEdit — éditeur graphique de texte et d'hexadécimal

## SYNOPSIS

`TextEdit`

## DESCRIPTION

Modifie n'importe quel fichier dans une fenêtre de bureau : texte, code
source, fichiers de réglages du système ou octets bruts. Lancé avec un
document — depuis le gestionnaire de fichiers, depuis le bureau, ou en
déposant un fichier sur son icône dans la barre d'icônes — il ouvre une
fenêtre sur ce fichier. Lancé seul, il ouvre une fenêtre vide. Chaque
document est une fenêtre de l'unique éditeur ; fermer la dernière le laisse
dans la barre d'icônes, et la ligne « Quitter » du menu de son icône y met
fin.

Rien de ce que contient un fichier n'est caché. Un octet de contrôle
s'affiche `[x03]`, un octet qui n'est pas de l'UTF-8 valide `[xC3]`, et un
caractère invisible ou qui change le sens d'écriture `[U+202E]`, chacun dans
sa propre couleur et chacun un seul pas du curseur. Un fichier qui ressemble à
des données binaires s'ouvre dans la vue hexadécimale, qui montre chaque octet
en deux chiffres hexadécimaux à côté de son caractère et modifie les mêmes
octets que la vue texte.

Un exécutable — une image de programme TAIRiX, un pilote, un fichier ELF64
ou un module WebAssembly — s'ouvre plutôt dans la vue code : ce que disent
ses en-têtes, chaque partie à sa position avec sa longueur et ce qu'elle
déclare — la version du système pour laquelle il a été construit, son point
d'entrée, ses segments et les bibliothèques dont il a besoin, le manifeste
signé d'un pilote avec les capacités qu'il demande et le matériel auquel il
se lie, les en-têtes de programme et de section d'un fichier ELF, les
sections d'un module —, puis les instructions de chaque région de code, une
par ligne avec son adresse et ses octets d'encodage, étiquetées des symboles
qu'elles commencent et des symboles que leurs branchements atteignent. Les
clés et les signatures sont montrées telles qu'elles sont écrites, sans être
vérifiées. La vue code est en lecture seule ; les vues texte et hexadécimale
modifient le même fichier. Le code est décodé par un second processus de
travail qui lui est propre, sans aucun accès lui non plus, et lu dans le jeu
d'instructions que nomme le fichier, sinon dans celui de cette machine ;
« Jeu d'instructions » dans le menu Affichage le lit dans un autre, et le
champ de vue de la ligne d'état propose la vue code à côté du texte et de
l'hexadécimal.

Une image — PNG, JPEG, GIF, BMP, TIFF, WEBP, un fichier d'icône ou de
pointeur, une zone de sprites RISC OS ou un document OpenRaster — s'ouvre à
la place dans la vue des données : ce dont le fichier est fait, chaque
enregistrement à sa position avec sa longueur et ce qu'il déclare, chaque
balise EXIF par son nom avec sa valeur, puis chaque octet du fichier en
hexadécimal à côté de son caractère. Un manifeste signé — l'`AppInfo` d'une
application ou le manifeste d'un programme — s'y ouvre aussi : ce qu'il
nomme, les capacités qu'il demande, les types de fichiers qu'il revendique
et les services qu'il recherche, puis ses octets. La vue des données est en
lecture seule ; la vue hexadécimale modifie les mêmes octets. Une image est
lue par un troisième processus de travail qui lui est propre, un manifeste
par celui de la vue code, aucun des deux n'ayant le moindre accès, et le
menu Affichage propose la vue des données à côté du texte et de
l'hexadécimal.

Le code source est coloré : HTML, XML et SVG, CSS, JavaScript, JSON, YAML,
TOML, Markdown, Rust, C, Java, Python et scripts shell. Les fichiers de
réglages du système — réglages d'application, bibliothèque de programmes,
configuration du système et du réseau, surcharges de services, bases des
utilisateurs et des groupes, et manifestes de familles de polices — sont
colorés eux aussi et vérifiés avec l'analyseur qui sert au système à les
lire : un problème est signalé dans la marge à côté de sa ligne et énoncé
dans la ligne d'état. Le format est choisi d'après le nom du fichier, puis
d'après ses premiers octets ; un format choisi dans le menu Affichage ou
dans la ligne d'état l'emporte toujours.

L'éditeur ne détient aucune capacité sur le système de fichiers. Il ne
modifie que le fichier qui lui a été remis. Un fichier que l'utilisateur peut
modifier est remis en écriture, et Enregistrer l'écrit en retour ; tout autre
est en lecture seule, et Enregistrer demande où enregistrer une copie. La
coloration, la détection du format et la vérification s'exécutent dans un
processus de travail séparé sans aucun accès, de sorte qu'un fichier hostile
ne peut rien atteindre de ce que l'éditeur atteint.

Appuyer sur le bouton secondaire (droit) de la souris n'importe où dans la
fenêtre ouvre son menu : Couper, Copier, Coller et Tout sélectionner, puis
Fichier, Édition, Rechercher et Affichage, chacun ouvrant son propre
sous-menu. La fenêtre n'a pas de barre de menus.

La ligne d'état indique la ligne et la colonne du curseur, ce que la
vérification a trouvé et, sous forme de champs qui ouvrent un menu au clic :
le format, texte ou hexadécimal, les fins de ligne et l'indentation. Fermer
une fenêtre ou quitter avec des modifications non enregistrées demande
d'abord confirmation. Fermer une fenêtre en cliquant sur son bouton de
fermeture avec le bouton secondaire (droit) montre aussi son fichier dans
le gestionnaire de fichiers, dans le dossier qui le contient.

* `Ctrl+N` — une nouvelle fenêtre
* `Ctrl+O` — ouvrir un fichier
* `Ctrl+S` — enregistrer ; `Ctrl+Shift+S` — enregistrer sous
* `Ctrl+W` — fermer la fenêtre
* `Ctrl+Z` — annuler ; `Ctrl+Shift+Z` ou `Ctrl+Y` — rétablir
* `Ctrl+X`, `Ctrl+C`, `Ctrl+V` — couper, copier, coller
* `Ctrl+A` — tout sélectionner
* `Ctrl+F` — rechercher ; `Ctrl+H` — remplacer
* `F3` / `Shift+F3` — l'occurrence suivante ou précédente
* `Ctrl+L` — aller à une ligne
* `F8` — le problème suivant trouvé par la vérification
* `Ctrl+]` / `Ctrl+[` — indenter ou désindenter les lignes sélectionnées
* `Ctrl+/` — commenter les lignes sélectionnées ou les décommenter
* `Ctrl+Shift+H` — passer de la vue texte à la vue hexadécimale et retour
* `Insert` — passer de l'insertion à la refrappe et retour
* `Tab` — dans la vue hexadécimale, passer des colonnes hexadécimales aux caractères

## OPTIONS

`-h`, `-?`, `--help`
: Écrire cette aide sur la sortie standard et quitter.

## EXIT STATUS

Zéro après Quitter. Non nul lorsque le canal de fenêtre, la boîte aux lettres
d'événements ou la session de bureau a été refusée ; la raison est énoncée
sur la sortie d'erreur standard.
