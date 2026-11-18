## NAME

OpenMicro — Open Micro Emulator, émulateur de micro-ordinateurs 8 bits

## SYNOPSIS

`OpenMicro`

## DESCRIPTION

Fait tourner des micro-ordinateurs 8 bits dans des fenêtres du bureau — le BBC
Micro Model B, l'Acorn Electron et le Commodore 64 — cycle pour cycle à leur
propre vitesse, chacun équipé de son propre système d'exploitation et de son
BASIC écrits en salle blanche, et, sur le Model B, d'un système de fichiers sur
disquette et, sur le Commodore 64, d'un lecteur de disques 1541 doté de son
propre DOS, si bien qu'une machine démarre à l'invite du BASIC. L'image est
celle d'un téléviseur, quatre unités de large pour trois de haut, et le son
passe par l'audio du bureau.

La fenêtre de bibliothèque montre chaque machine sous forme de vignette de son
dernier écran ou, tant qu'elle n'a pas encore tourné, de l'écran de démarrage
de son modèle. Nouveau crée une machine, sous le nom demandé : un BBC Micro
Model B, avec de la RAM latérale dans les emplacements 4 à 7 et une nouvelle
disquette vierge dans le lecteur 0 ou sans disquette ; un Acorn Electron,
équipé ou non d'un Plus 1 ; ou un Commodore 64, PAL ou NTSC, avec un SID 6581
ou 8580 et un lecteur 1541, deux ou aucun. Démarrer, un double-clic ou la
touche Entrée démarre la machine choisie dans une fenêtre à elle, ou fait
passer sa fenêtre au premier plan ; jusqu'à vingt machines sont conservées, et
plusieurs tournent à la fois. Dupliquer copie une machine avec son état
enregistré et son dossier, et Supprimer supprime une machine et son état
enregistré une fois la suppression confirmée, en laissant son dossier. Un clic
droit sur la vignette d'une machine propose Démarrer, Réglages, Dupliquer,
Supprimer et Afficher le dossier pour cette machine et, tant qu'elle tourne,
le menu de la machine aussi. Au premier lancement, l'émulateur crée un BBC
Micro Model B avec une disquette vierge.

Réglages change le nom d'une machine, son volume, le chargement rapide des
cassettes, qui fait tourner la machine aussi vite que possible tant que le
moteur du magnétophone tourne, et, sur un Commodore 64, le port de manette où
est branché son joystick au pavé numérique. Ce qui y est installé ne change que
lorsqu'elle est éteinte : la ROM de chaque support — l'OS et les seize
emplacements latéraux du Model B, l'OS et le BASIC de l'Electron, la ROM et les
cartouches de son Plus 1, et, sur le Commodore 64, le KERNAL, le BASIC, le jeu
de caractères et le DOS de chaque lecteur —, chacune pouvant être une ROM
livrée, de la RAM latérale dans un emplacement du Model B, rien dans un
emplacement ou une cartouche, ou une image de ROM à vous, placée dans le
magasin de l'émulateur avec Installer une image ; le Plus 1 de l'Electron ; la
norme vidéo, le SID et les lecteurs du Commodore 64 ; et les cavaliers du
clavier du Model B, qui fixent son mode d'écran au démarrage, la façon dont une
disquette s'amorce et les temporisations de disquette.

Un Model B a aussi VIM, un éditeur dans le style de vi, installé dans
l'emplacement 13. À l'invite de la machine, `*VIM` suivi du nom d'un fichier
édite ce fichier, et BREAK dans VIM garde le texte qu'il contient. VIM édite du
texte, comme les fichiers `!BOOT` que crée `*BUILD`, et des programmes BASIC,
qu'il liste sous forme de texte et réenregistre tokenisés, en colorant chacun
autant que le permettent les couleurs du mode d'écran ; `:dis` suivi du nom
d'un fichier fait défiler page par page le code machine d'un programme. Dans
VIM, `:help` liste ses commandes et les modes d'écran avec la place que laisse
chacun, `:mode` suivi d'un nombre change de mode, et `:q` revient au BASIC. Là
où une banque de RAM latérale ne contient aucune ROM, VIM y garde ce qu'il copie
ou supprime, dans la plus haute de ces banques, si bien qu'il ne prend rien de
la mémoire dont dispose le texte.

Fermer la fenêtre d'une machine la met en veille : redémarrée, elle reprend là
où elle s'était arrêtée. Éteindre, dans son menu, retire ses supports et
l'éteint, en ne gardant que son dernier écran.

Chaque machine a un dossier à elle, Open Micro puis le nom de la machine, dans
votre dossier Documents. Nouvelle disquette, Nouveau disque sur un Commodore
64, et Nouvelle cassette y créent une disquette, un disque ou une cassette
vierge et l'insèrent, Depuis le dossier propose les disquettes, les disques et
les cassettes qui s'y trouvent déjà, et Enregistrer un instantané y garde
toute la machine dans un fichier `.snapshot`. Afficher le dossier ouvre le
dossier dans Files, si bien que ce que la machine y garde peut être copié sur
d'autres supports ou sur un autre ordinateur. Ce que ses lecteurs et son
magnétophone contiennent de son dossier reste avec la machine : éteinte, elle
le réinsère quand elle démarre ; sortie de veille, elle ne le réécrit que là
où aucun autre programme ne l'a modifié depuis sa mise en veille.

Un fichier ouvert avec l'émulateur — une image de disquette (`.ssd`, `.dsd`) ou
de cassette (`.uef`, `.csw`), ou, pour un Commodore 64, un disque (`.d64`,
`.g64`), une cassette (`.tap`), une cartouche (`.crt`) ou un programme (`.prg`,
`.t64`) — va à la machine en marche dont la fenêtre a été active en dernier, ou
à une autre machine en marche qui l'accepte, ou sinon à la première machine de
la bibliothèque qui l'accepte, démarrée pour l'occasion, et la fenêtre de
cette machine passe au premier plan. Ouvrir une disquette de BBC Micro dans
Files montre les fichiers qu'elle contient au lieu de démarrer une machine.
Celui qui est choisi avec Insérer une disquette, Insérer un disque ou Insérer
une cassette va dans ce lecteur ou dans le magnétophone, et celui qui est
déposé sur la fenêtre d'une machine va dans son premier lecteur ou le
magnétophone, protégé en écriture, la fenêtre prenant le clavier. Ce que la
machine écrit sur une disquette ou un disque retourne dans son fichier tant
que ce fichier peut être écrit, et ni un montage ni une autre machine ne
peuvent écrire ce fichier tant que le support est dans le lecteur ; une
disquette ou un disque dont le fichier ne peut pas l'être, ou qu'un autre
programme utilise, est protégé en écriture, et la fenêtre en donne la raison.
Pour un disque `.d64` qui a fini par exiger un `.g64`, il est proposé de
l'enregistrer sous ce format. Éjecter retire une disquette ou un disque, et
Enregistrer la disquette sous ou Enregistrer le disque sous en garde une copie
où l'on veut. Une cassette sur laquelle on a enregistré est sauvegardée quand
on la retire. Sur un Commodore 64, Lire, Enregistrer, Arrêter et Rembobiner,
dans le menu Cassette, commandent sa datasette ; Insérer une cartouche en
branche une et Retirer la cartouche l'enlève, l'un comme l'autre redémarrant
la machine ; et Charger un programme met un programme en mémoire, comme le
ferait LOAD, pour que RUN le lance. Enregistrer un instantané garde toute la
machine dans un fichier — dans son dossier ou, tant qu'elle n'en a pas, à
l'endroit de votre choix —, et Restaurer un instantané reprend une machine
ainsi gardée, avec ses supports.

Le menu, ouvert d'un clic droit sur l'image, propose chaque lecteur d'un Model
B ou d'un Commodore 64 et la cassette, ainsi que, sur un Commodore 64, la
cartouche et Charger un programme ; BREAK, ou RESTORE sur un Commodore 64 ;
sur un Model B et un Electron, SHIFT-BREAK, qui appuie sur BREAK en maintenant
SHIFT pour amorcer la disquette ou la cassette ; Réinitialiser, Pause et la
vitesse ; le plein écran, une mise à l'échelle nette ou lissée et les lignes
de balayage ; la correspondance du clavier ; Coller, qui tape le texte du
presse-papiers, chaque lettre dans la casse qu'affiche la machine ; les
instantanés ; Afficher le dossier ; Réglages ; Bibliothèque de machines ; et
Éteindre. Par caractère, les touches tapent ce qu'elles montrent, le CAPS LOCK
de la machine, ou sur un Commodore 64 la touche Maj, décidant de la casse
d'une lettre. Par position, chaque touche est la touche de la machine à sa
place. Sur un Model B, F1 à F9 sont f1 à f9, F10 est f0, Début est SHIFT LOCK
et Inser est la touche barre oblique inversée. Sur un Electron, F1 à F10 sont
FUNC avec 1 à 9 et 0, Alt est FUNC, et Verr Maj est CAPS LK. Sur les deux, F12
est BREAK, Fin est COPY et les flèches sont les touches de curseur. Sur un
Commodore 64, F1, F3, F5 et F7 sont ses touches de fonction et F2, F4, F6 et
F8 les mêmes avec SHIFT, Échap est RUN/STOP, Alt est la touche Commodore, Ctrl
est CTRL, Début est CLR/HOME, F12 ou Page préc. est RESTORE, et les flèches
sont les touches de curseur ; quand son joystick au pavé numérique est activé,
les flèches du pavé numérique déplacent le joystick, et les touches 0 et 5
tirent.

Des voyants au bas de la fenêtre d'une machine montrent CAPS LOCK et le moteur
du magnétophone, et, sur un Model B, SHIFT LOCK et chaque lecteur ; sur un
Commodore 64, ils montrent le moteur de la datasette et chaque lecteur. Sur un
Model B et un Electron, le bouton SHIFT-BREAK, en bas à droite, fait ce que
fait la ligne du menu. Chaque machine tourne dans son propre processus
auxiliaire, qui ne détient aucune autorité, si bien qu'on ne fait confiance ni
à ses logiciels ni à ses supports ; un processus qui échoue est remplacé, la
machine repartant de la mise sous tension, et la raison est affichée.

Fermer la fenêtre de bibliothèque laisse tourner les machines en marche, et
fermer toutes les fenêtres laisse l'émulateur dans la barre d'icônes : un clic
sur son icône, ou Bibliothèque de machines dans son menu, fait toujours passer
la bibliothèque au premier plan, et Quitter dans ce menu met en veille toutes
les machines en marche et met fin à l'émulateur.

## OPTIONS

`-h`, `-?`, `--help`
: Écrit cette aide sur la sortie standard et quitte.

## EXIT STATUS

Zéro une fois Quitter choisi dans le menu de son icône de la barre d'icônes.
Non nul quand le canal de fenêtre, la boîte aux lettres d'événements ou la
session du bureau ont été refusés ; la raison est indiquée sur la sortie
d'erreur standard.
