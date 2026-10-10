# Teste manual do Aionix Connect (build de validação)

Build de **desenvolvimento**, não de produção. Tudo roda nesta estação Windows.

## Abrir

Atalho **Aionix Connect Dev Launcher** (raiz do repositório ou Menu Iniciar →
"Aionix Connect (desenvolvimento)") → **Iniciar tudo**.

O launcher sobe o servidor (Docker quando disponível; senão o modo Nativo), espera os
serviços ficarem prontos, cria a conta de teste e abre o **Aionix Connect** — um app só:
à esquerda, **Este computador** (o ID de 9 dígitos); à direita, **Acessar outro
computador**, já conectado à conta de teste.

## Sessão neste mesmo computador

1. Em **Seus dispositivos**, clique em **Conectar** no dispositivo marcado "este computador".
2. Na janela "Um técnico está solicitando acesso a este computador." clique **Permitir**.
3. A tela remota ocupa a janela (efeito "túnel", pois é a própria tela). O **Controle** começa
   desligado aqui de propósito: com os dois lados na mesma máquina o cursor injetado
   perseguiria a janela do Técnico.
4. **Diagnóstico** mostra transporte, latência, fps, resolução, codificador/decodificador.
5. Encerre pela barra da sessão (**Encerrar**) ou em Este computador (**Encerrar sessão**).

## Teste com outro computador (outra rede)

1. No launcher marque **Testar pela internet com outro computador** → **Iniciar tudo**.
   Um túnel HTTPS temporário (Cloudflare) expõe o servidor local; o endereço muda a cada
   início.
2. Envie `dist\Aionix-Connect-Cliente.zip` (botão **Abrir pasta do pacote**) para a outra
   pessoa. Ela extrai, abre **Aionix Connect.exe** (o Windows SmartScreen pode avisar: "Mais
   informações" → "Executar assim mesmo") e informa o ID.
3. Em **Acessar outro computador**, digite o ID → a outra pessoa clica **Permitir**.
4. Ao terminar, **Parar servidor** no launcher (fecha o túnel).

Não reinicie o launcher entre enviar o pacote e o teste: um novo túnel tem outro endereço
e o pacote antigo deixa de funcionar.

## Registros

`.data\dev\logs` (launcher e scripts), `.data\native\logs` (servidor nativo),
`.data\apps\...\logs` (aplicativos). O atendido do pacote grava em
`%LOCALAPPDATA%\Aionix Connect\host\logs`.
