## NAME

netmap — carte et inventaire du réseau

## SYNOPSIS

`netmap`

## DESCRIPTION

Ouvre une fenêtre qui cartographie les réseaux auxquels cette machine est
reliée : ses interfaces réseau, les sous-réseaux où elles se trouvent, les
hôtes que son cache de voisinage a vus sur chacun, les routeurs par lesquels
passent ses routes et la passerelle par défaut qui mène au-delà du réseau
local. Chaque élément est dessiné comme une icône sur un disque de la couleur
de ce qu'il est — ordinateurs en bleu, routeurs en orange, imprimantes en
violet, stockage en bleu-vert, téléphones en vert —, et chaque trait prend la
couleur de ce vers quoi il mène. Un hôte dont la carte reconnaît le système
d'exploitation en porte l'emblème, et un voyant à côté d'un hôte ou d'une
interface indique s'il était joignable lors de la dernière observation (vert)
ou non (rouge).

Faites glisser avec le bouton principal, tournez la molette ou faites défiler
à deux doigts sur un pavé tactile pour vous déplacer sur la carte. Maintenez
`Ctrl` en tournant la molette, ou pincez sur un pavé ou un écran tactile, pour
zoomer autour du point sous le pointeur ; appuyez sur `Home` pour faire tenir
de nouveau toute la carte dans la fenêtre. Cliquez sur un nœud pour voir ce
que l'on sait de lui — ses adresses, son adresse matérielle et son fabricant,
sa joignabilité, le type d'appareil qu'il semble être et ce qu'il exécute,
avec le degré de certitude de la carte et ses raisons — et cliquez dans le
vide pour ranger ces détails. Les noms apparaissent partout où ils ont la
place.

Un hôte qui annonce des services en porte le nombre sur un petit badge.
Laissez le pointeur sur un nœud pour voir son nom complet, et sur un tel hôte
pour voir ses services nommés ; cliquez sur l'hôte pour déployer ses services
autour de lui, puis cliquez de nouveau dessus, cliquez ailleurs ou appuyez sur
`Escape` pour les replier. Cliquez sur un nœud avec le bouton secondaire pour
ouvrir son menu : afficher ou masquer ses services, ou copier son adresse, son
nom ou son adresse matérielle — pour un service, son adresse et son port, et
pour un service web, son URL.

La carte est lue au travers de l'API d'information système et du service de
découverte, et ses hôtes sont nommés par les serveurs DNS configurés ou par le
nom que les appareils se donnent eux-mêmes ; elle ne contacte jamais elle-même
un hôte, et elle est relue toutes les quelques secondes tant que sa fenêtre
est ouverte. Elle recueille aussi les services que les appareils annoncent sur
le lien local — imprimantes, partages de fichiers, lecteurs multimédias — et
range chacun avec l'hôte qui l'offre. Les adresses d'interface et les routes
exigent `CAP_SYSINFO_GLOBAL` ; les adresses matérielles et le cache des
voisins exigent `CAP_SYSINFO_HW` ; les services annoncés exigent
`CAP_NET_DISCOVER_ALL` ; nommer les hôtes par DNS exige `CAP_NET`. Sans l'une
d'elles, la carte dit ce qu'elle ne peut pas montrer et dessine le reste.

Netmap se lance depuis la Bibliothèque de programmes du bureau ou par son nom
depuis un shell, et exige une session graphique active ; sans elle, il signale
le refus sur le flux d'erreur standard et se termine.

## EXIT STATUS

Zéro après Quitter ; non nul lorsque le canal de fenêtre ou la région d'image
partagée a été refusé (la raison est indiquée sur le flux d'erreur standard).
