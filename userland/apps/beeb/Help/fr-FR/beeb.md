## NAME

beeb — émulateur de BBC Micro modèle B

## SYNOPSIS

`beeb`

## DESCRIPTION

Fait tourner un BBC Micro modèle B dans une fenêtre du bureau, cycle pour
cycle à sa propre vitesse, équipé de son MOS, de son BBC BASIC et de son DFS
écrits en salle blanche : il démarre à l'invite du BASIC avec un système de
fichiers sur disquette. L'image est celle d'un téléviseur, quatre unités de
large pour trois de haut, et le son passe par l'audio du bureau.

Une image de disquette (`.ssd`, `.dsd`) ou de cassette (`.uef`, `.csw`)
ouverte avec l'émulateur, déposée sur sa fenêtre ou choisie avec Insérer une
disquette ou Insérer une cassette dans son menu entre dans le lecteur ou dans
le magnétophone. Ce que la machine écrit sur une disquette retourne dans le
fichier de la disquette tant que ce fichier peut être écrit ; une disquette
dont le fichier ne peut pas l'être est protégée en écriture. Éjecter retire
une disquette, et Enregistrer la disquette sous en garde une copie où l'on
veut. Une cassette sur laquelle on a enregistré est sauvegardée quand on la
retire. Enregistrer un instantané garde toute la machine dans un fichier, et
Restaurer un instantané la reprend, avec ses disquettes et sa cassette.

Le menu, ouvert d'un clic droit sur l'image ou depuis l'icône de l'émulateur
dans la barre d'icônes, propose aussi BREAK, Réinitialiser, Pause et la
vitesse ; le plein écran, une mise à l'échelle nette ou lissée et les lignes
de balayage ; la correspondance du clavier ; et Coller, qui tape le texte du
presse-papiers. Par caractère, les touches tapent ce qu'elles montrent, le
CAPS LOCK de la machine décidant de la casse d'une lettre. Par position,
chaque touche est la touche BBC à sa place : F1 à F9 sont f1 à f9, F10 est
f0, F12 est BREAK, Fin est COPY et Début est SHIFT LOCK.

Réglages choisit ce que contiennent le support du système et les seize
emplacements latéraux — les ROM livrées, de la RAM latérale ou des images de
ROM de l'utilisateur, installées dans le magasin de l'émulateur —, le mode
d'écran, le démarrage et les temporisations de disquette des cavaliers du
clavier, l'image, les touches, le volume et le chargement rapide des
cassettes, qui fait tourner la machine aussi vite que possible tant que le
moteur du magnétophone tourne. Les changements des ROM et des cavaliers
prennent effet à la prochaine réinitialisation.

Des voyants au bas de la fenêtre montrent CAPS LOCK, SHIFT LOCK, le moteur du
magnétophone et chaque lecteur. La machine tourne dans un processus auxiliaire
qui ne détient aucune autorité : ni ses logiciels ni ses supports ne sont
dignes de confiance ; un processus qui échoue est remplacé, la machine
repartant de la mise sous tension, et la raison est affichée.

## OPTIONS

`-h`, `-?`, `--help`
: Écrit cette aide sur la sortie standard et quitte.

## EXIT STATUS

Zéro une fois sa fenêtre fermée ou Quitter choisi dans le menu de son icône de la barre d'icônes. Non nul quand le canal de fenêtre, la boîte aux lettres d'événements ou la session du bureau ont été refusés ; la raison est indiquée sur la sortie d'erreur standard.
