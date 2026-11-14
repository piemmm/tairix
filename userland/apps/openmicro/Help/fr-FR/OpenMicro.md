## NAME

OpenMicro — Open Micro Emulator, émulateur de micro-ordinateurs 8 bits

## SYNOPSIS

`OpenMicro`

## DESCRIPTION

Fait tourner des micro-ordinateurs 8 bits dans des fenêtres du bureau — le BBC
Micro Model B et l'Acorn Electron — cycle pour cycle à leur propre vitesse,
chacun équipé de son propre système d'exploitation et de son BASIC écrits en
salle blanche, et, sur le Model B, d'un système de fichiers sur disquette, si
bien qu'une machine démarre à l'invite du BASIC. L'image est celle d'un
téléviseur, quatre unités de large pour trois de haut, et le son passe par
l'audio du bureau.

La fenêtre de bibliothèque montre chaque machine sous forme de vignette de son
dernier écran. Nouveau crée une machine, sous le nom demandé : un BBC Micro
Model B, avec une nouvelle disquette vierge dans le lecteur 0 ou sans
disquette, ou un Acorn Electron, équipé ou non d'un Plus 1. Démarrer, un
double-clic ou la touche Entrée démarre la machine choisie dans une fenêtre à
elle, ou fait passer sa fenêtre au premier plan ; jusqu'à vingt machines sont
conservées, et plusieurs tournent à la fois. Dupliquer copie une machine avec
son état enregistré et son dossier, et Supprimer supprime une machine et son
état enregistré une fois la suppression confirmée, en laissant son dossier. Un
clic droit sur la vignette d'une machine propose Démarrer, Réglages, Dupliquer
et Supprimer pour cette machine et, tant qu'elle tourne, le menu de la machine
aussi. Au premier lancement, l'émulateur crée un BBC Micro Model B avec une
disquette vierge.

Réglages change le nom d'une machine, son volume et le chargement rapide des
cassettes, qui fait tourner la machine aussi vite que possible tant que le
moteur du magnétophone tourne. Ce qui y est installé ne change que lorsqu'elle
est éteinte : la ROM de chaque support — l'OS et les seize emplacements
latéraux du Model B, l'OS et le BASIC de l'Electron, la ROM et les cartouches
de son Plus 1 —, chacune pouvant être une ROM livrée, de la RAM latérale dans
un emplacement du Model B, rien dans un emplacement ou une cartouche, ou une
image de ROM à vous, placée dans le magasin de l'émulateur avec Installer une
image ; le Plus 1 de l'Electron ; et les cavaliers du clavier du Model B, qui
fixent son mode d'écran au démarrage, la façon dont une disquette s'amorce et
les temporisations de disquette.

Fermer la fenêtre d'une machine la met en veille : redémarrée, elle reprend là
où elle s'était arrêtée. Éteindre, dans son menu, retire ses disquettes et sa
cassette et l'éteint, en ne gardant que son dernier écran.

Chaque machine a un dossier à elle, Open Micro puis le nom de la machine, dans
votre dossier Documents. Nouvelle disquette et Nouvelle cassette y créent une
disquette ou une cassette vierge et l'insèrent, et Depuis le dossier propose
les disquettes et les cassettes qui s'y trouvent déjà. Ce que ses lecteurs et
son magnétophone contiennent de son dossier reste avec la machine : éteinte,
elle le réinsère quand elle démarre ; sortie de veille, elle ne le réécrit que
là où aucun autre programme ne l'a modifié depuis sa mise en veille.

Une image de disquette (`.ssd`, `.dsd`) ou de cassette (`.uef`, `.csw`) ouverte
avec l'émulateur va à la machine en marche dont la fenêtre a été active en
dernier, ou à une autre machine en marche qui l'accepte, ou sinon à la première
machine de la bibliothèque qui l'accepte, démarrée pour l'occasion. Celle qui
est choisie avec Insérer une disquette ou Insérer une cassette va dans ce
lecteur ou dans le magnétophone, et celle qui est déposée sur la fenêtre d'une
machine va dans le lecteur 0 ou le magnétophone, protégée en écriture. Ce que
la machine écrit sur une disquette retourne dans le fichier de la disquette
tant que ce fichier peut être écrit ; une disquette dont le fichier ne peut pas
l'être est protégée en écriture. Éjecter retire une disquette, et Enregistrer
la disquette sous en garde une copie où l'on veut. Une cassette sur laquelle on
a enregistré est sauvegardée quand on la retire. Enregistrer un instantané
garde toute la machine dans un fichier, et Restaurer un instantané la reprend,
avec ses disquettes et sa cassette.

Le menu, ouvert d'un clic droit sur l'image, propose chaque lecteur d'un Model
B et la cassette ; BREAK, Réinitialiser, Pause et la vitesse ; le plein écran,
une mise à l'échelle nette ou lissée et les lignes de balayage ; la
correspondance du clavier ; Coller, qui tape le texte du presse-papiers ; les
instantanés ; Réglages ; Bibliothèque de machines ; et Éteindre. Par caractère,
les touches tapent ce qu'elles montrent, le CAPS LOCK de la machine décidant de
la casse d'une lettre. Par position, chaque touche est la touche de la machine
à sa place. Sur un Model B, F1 à F9 sont f1 à f9, F10 est f0, Début est SHIFT
LOCK et Inser est la touche barre oblique inversée. Sur un Electron, F1 à F10
sont FUNC avec 1 à 9 et 0, Alt est FUNC, et Verr Maj est CAPS LK. Sur les deux,
F12 est BREAK, Fin est COPY et les flèches sont les touches de curseur.

Des voyants au bas de la fenêtre d'une machine montrent CAPS LOCK et le moteur
du magnétophone, et, sur un Model B, SHIFT LOCK et chaque lecteur. Chaque
machine tourne dans son propre processus auxiliaire, qui ne détient aucune
autorité, si bien qu'on ne fait confiance ni à ses logiciels ni à ses
supports ; un processus qui échoue est remplacé, la machine repartant de la
mise sous tension, et la raison est affichée.

Fermer la fenêtre de bibliothèque laisse tourner les machines en marche, et
fermer toutes les fenêtres laisse l'émulateur dans la barre d'icônes : un clic
sur son icône, ou Bibliothèque de machines dans son menu, rouvre la
bibliothèque, et Quitter dans ce menu met en veille toutes les machines en
marche et met fin à l'émulateur.

## OPTIONS

`-h`, `-?`, `--help`
: Écrit cette aide sur la sortie standard et quitte.

## EXIT STATUS

Zéro une fois Quitter choisi dans le menu de son icône de la barre d'icônes.
Non nul quand le canal de fenêtre, la boîte aux lettres d'événements ou la
session du bureau ont été refusés ; la raison est indiquée sur la sortie
d'erreur standard.
