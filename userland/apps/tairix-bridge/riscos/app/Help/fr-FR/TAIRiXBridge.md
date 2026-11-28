## NAME

TAIRiXBridge — TAIRiX Bridge, des programmes RISC OS sur le bureau TAIRiX

## SYNOPSIS

`TAIRiXBridge`

## DESCRIPTION

Exécute des programmes RISC OS — programmes BASIC, outils en ligne de
commande et jeux monotâches — sur le bureau TAIRiX, chacun montrant son écran
dans une fenêtre à lui. Le RISC OS sur lequel ils tournent est celui de TAIRiX
Bridge, écrit d'après la documentation publiée ; il ne contient aucun code de
RISC OS.

Les programmes tournent dans des sessions. La session partagée est la machine
RISC OS que montre la fenêtre TAIRiX Bridge : son propre disque est le dossier
RISC OS de vos Documents, sa séquence de démarrage s'exécute à son lancement,
et un programme venu de votre propre stockage y tourne. Un programme venu
d'ailleurs — support amovible, image de disque, partage réseau — ou d'une
origine inconnue tourne dans une session isolée à lui, qui ne voit jamais votre
dossier RISC OS et se termine avec son dernier programme. Exécuter dans la
session partagée exécute un fichier dans la session partagée, d'où qu'il
vienne.

La fenêtre d'une session liste ses tâches, chacune avec sa mémoire et si elle
est en pause ou occupée, et ce qu'occupent les zones partagées de la session. Exécuter
choisit un fichier à exécuter ; un fichier déposé sur la fenêtre, ou ouvert
avec TAIRiX Bridge, s'exécute aussi. Choisir une tâche puis Pause ou Quitter
la suspend ou la termine.

L'écran de chaque programme est une fenêtre à lui, son image mise à l'échelle
de la fenêtre dans les proportions de son mode. Le bouton Écran ouvre le menu
de la fenêtre : Plein écran, mise à l'échelle nette ou lissée, Pause, et
Réinitialiser, qui termine le programme et le relance dans la même fenêtre.
L'écran prend tout le clavier, chaque touche avec le numéro que RISC OS lui
donne. Select, Menu et Adjust sont les boutons principal, secondaire et du
milieu. Dès qu'un programme déplace lui-même la souris, un clic la capture
pour lui, et une seule pression de la touche Méta la libère.

Réglages choisit l'ordre des boutons, la mémoire que chaque session peut
prendre, si chaque programme est isolé, et comment les écrans sont mis à
l'échelle. Le processus de travail d'une session ne détient aucune autorité :
les fichiers qu'il lit et écrit sont transmis pour lui, et un processus qui
échoue termine sa session, la raison étant affichée dans la fenêtre de la
session.

## OPTIONS

`-h`, `-?`, `--help`
: Écrire cette aide sur la sortie standard et quitter.

## EXIT STATUS

Zéro une fois la fenêtre de la dernière session fermée. Non nul lorsque le
canal de fenêtres, la boîte d'événements ou la session de bureau a été
refusé ; la raison est indiquée sur le flux d'erreur standard.
