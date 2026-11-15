// Qual som vai junto com a live de tela, por padrão.
//
// No app, o padrão é só o som do jogo ou programa escolhido (como o Discord faz). Com o som do
// computador inteiro, a voz de quem está na chamada com você entra na live e a pessoa se ouve com
// atraso: aconteceu em 15/11/2026, com o padrão antigo. Ao transmitir a tela inteira não há programa
// escolhido, e aí vai o som do computador (ver `captureScreenNative`).
//
// No navegador o padrão continua sendo o som do computador: é o único que o navegador oferece ao
// compartilhar a tela inteira.
export type AudioChoice = 'window' | 'system' | 'none';

export function defaultAudio(app: boolean): AudioChoice {
  return app ? 'window' : 'system';
}

// Uma vez só, no app: quem estava no padrão antigo ("Computador") passa para o novo. Não dá para
// distinguir quem escolheu "Computador" de quem nunca mexeu, e o eco é o problema maior; quem
// escolher "Computador" depois disso fica com a escolha.
export function migrateAudio(audio: AudioChoice, app: boolean, migrated: boolean): AudioChoice {
  return app && !migrated && audio === 'system' ? 'window' : audio;
}
