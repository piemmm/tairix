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
d'abord confirmation.

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
